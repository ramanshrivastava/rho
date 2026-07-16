"""Deterministic normalizer for agent JSON event streams.

The crosscheck compares tau's ``-p --output json`` event stream against rho's. The
two agents emit semantically identical streams that differ only in *volatile*
values — random UUIDs (entry ids, tool-call ids, response ids) and wall-clock
timestamps. This module rewrites those volatile values to stable, position-based
tokens so the two streams can be diffed for structural equality.

It is intentionally standalone (no tau import) so the rho side can reuse the exact
same normalization at M4a when it plugs into the crosscheck.

Normalization rules
-------------------
* id-like fields (``id``, ``parentId``/``parent_id``, ``toolCallId``/
  ``tool_call_id``, ``responseId``/``response_id``, ``entryId``/``entry_id``,
  ``fromId``/``from_id``, ``branchRootId``/``branch_root_id``, ``replacesEntryIds``)
  -> ``<id:N>`` assigned in first-seen order, consistently (so a parent reference
  resolves to the same token as the id it points at).
* timestamp-like fields (``timestamp``, ``createdAt``/``created_at``) ->
  ``<ts:N>`` assigned in first-seen order.

Everything else is preserved byte-for-byte, including key order.
"""

from __future__ import annotations

import json
from typing import Any

_ID_KEYS = {
    "id", "parentId", "parent_id", "toolCallId", "tool_call_id",
    "responseId", "response_id", "entryId", "entry_id", "fromId", "from_id",
    "branchRootId", "branch_root_id",
}
_ID_LIST_KEYS = {"replacesEntryIds", "replaces_entry_ids"}
_TS_KEYS = {"timestamp", "createdAt", "created_at"}


class StreamNormalizer:
    """Stateful normalizer; reuse one instance across a whole session stream."""

    def __init__(self) -> None:
        self._ids: dict[str, str] = {}
        self._ts: dict[str, str] = {}

    def _id_token(self, value: str) -> str:
        if value not in self._ids:
            self._ids[value] = f"<id:{len(self._ids)}>"
        return self._ids[value]

    def _ts_token(self, value: Any) -> str:
        key = repr(value)
        if key not in self._ts:
            self._ts[key] = f"<ts:{len(self._ts)}>"
        return self._ts[key]

    def normalize(self, value: Any, _key: str | None = None) -> Any:
        if isinstance(value, dict):
            return {k: self.normalize(v, k) for k, v in value.items()}
        if isinstance(value, list):
            if _key in _ID_LIST_KEYS:
                return [self._id_token(v) if isinstance(v, str) else v for v in value]
            return [self.normalize(v, _key) for v in value]
        if _key in _ID_KEYS and isinstance(value, str):
            return self._id_token(value)
        if _key in _TS_KEYS and isinstance(value, (int, float)) and not isinstance(value, bool):
            return self._ts_token(value)
        return value

    def normalize_line(self, line: str) -> str:
        obj = json.loads(line)
        return json.dumps(self.normalize(obj), ensure_ascii=False, separators=(",", ":"))


def normalize_stream(lines: list[str]) -> list[str]:
    """Normalize a full JSONL event stream with one shared token space."""
    normalizer = StreamNormalizer()
    return [normalizer.normalize_line(line) for line in lines if line.strip()]


if __name__ == "__main__":
    import sys

    out = normalize_stream(sys.stdin.read().splitlines())
    print("\n".join(out))
