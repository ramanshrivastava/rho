"""Shared helpers for deterministic golden-fixture extraction from tau.

Every fixture in ``rho/fixtures/`` is produced *by tau's own serialization code*
(``model_dump_json``, ``entry_to_json_line``, ``render_session_html``) so that the
files are an authoritative oracle for the Rust port, not a hand-written guess at
what tau emits.

Determinism
-----------
tau stamps wall-clock timestamps and random UUIDs into messages, session entries,
and HTML exports. To make ``just refresh-fixtures`` reproducible byte-for-byte we
monkeypatch the three non-deterministic sources at their definition sites *before*
constructing any model:

* ``tau_agent.messages.time``        -> fixed clock (int-millisecond message stamps)
* ``tau_agent.session.entries.time`` -> fixed clock (float session-entry stamps)
* ``tau_agent.session.entries.uuid4``-> a monotonic counter (entry ids)
* ``tau_coding.session_export.datetime`` -> frozen "generated at" clock

Because ``pydantic`` ``default_factory`` callables look their dependencies up as
module globals at call time, patching the *modules* (not the factory functions,
whose references pydantic has already captured) is what actually takes effect.

The counter resets every process, so extraction order is what pins ids; the
extractors construct fixtures in a fixed order and mostly pass explicit ids.
"""

from __future__ import annotations

import itertools
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

# --- Frozen clocks -----------------------------------------------------------

# Message timestamps are int milliseconds: int(time() * 1000).
# 1_700_000_000.123 -> 1_700_000_000_123 ms.
_FIXED_MESSAGE_TIME = 1_700_000_000.123

# Session-entry timestamps are floats. A whole-number float exercises the
# "1731234567.0 must round-trip as 1700000000.0, not 1700000000" quirk.
_FIXED_ENTRY_TIME = 1_700_000_000.0

# Frozen "generated at" instant for HTML session exports.
_FIXED_EXPORT_DT = datetime(2024, 1, 1, 0, 0, 0, tzinfo=UTC)


class _CounterUUID:
    """Stand-in for ``uuid.UUID`` exposing only the ``.hex`` tau reads."""

    __slots__ = ("hex",)

    def __init__(self, n: int) -> None:
        self.hex = f"{n:032x}"


def patch_determinism() -> None:
    """Freeze tau's clocks and uuid source. Idempotent; call once up front."""
    import tau_agent.messages as messages
    import tau_agent.session.entries as entries
    import tau_coding.session_export as session_export

    messages.time = lambda: _FIXED_MESSAGE_TIME  # type: ignore[assignment]
    entries.time = lambda: _FIXED_ENTRY_TIME  # type: ignore[assignment]

    counter = itertools.count()
    entries.uuid4 = lambda: _CounterUUID(next(counter))  # type: ignore[assignment]

    class _FrozenDateTime(datetime):
        @classmethod
        def now(cls, tz: Any = None) -> datetime:  # type: ignore[override]
            return _FIXED_EXPORT_DT if tz is None else _FIXED_EXPORT_DT.astimezone(tz)

    session_export.datetime = _FrozenDateTime  # type: ignore[assignment]


# --- Paths -------------------------------------------------------------------


def fixtures_root() -> Path:
    """Return ``rho/fixtures`` regardless of the process working directory."""
    root = Path(__file__).resolve().parents[2] / "fixtures"
    root.mkdir(parents=True, exist_ok=True)
    return root


def tau_rev() -> str:
    """Return the pinned tau git rev recorded in ``fixtures/TAU_REV``."""
    return (fixtures_root() / "TAU_REV").read_text(encoding="utf-8").strip()


# --- Writers -----------------------------------------------------------------


def write_text(rel: str, text: str) -> Path:
    """Write ``text`` verbatim to ``fixtures/<rel>``; return the path."""
    path = fixtures_root() / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def _key_sequence(value: Any) -> Any:
    """Recursively project an object to its key structure (order-sensitive)."""
    if isinstance(value, dict):
        return [(k, _key_sequence(v)) for k, v in value.items()]
    if isinstance(value, list):
        return [_key_sequence(v) for v in value]
    return None


def write_wire_json(rel: str, raw_json: str) -> Path:
    """Persist a tau-serialized wire JSON string *exactly* as tau emitted it.

    tau emits compact single-line JSON (pydantic ``model_dump_json`` /
    ``dump_json``). The golden file is that byte string verbatim plus a trailing
    newline — this is the authoritative oracle the Rust port must reproduce
    byte-for-byte, so we deliberately do **not** re-pretty-print it (that would
    risk float/spacing divergence from tau's actual output).

    Self-check: parse the payload and confirm the recursive *key order* survives a
    round trip. We compare key sequences, not values, because Python's
    ``json.dumps`` renders small floats in scientific notation (``5e-05``) whereas
    tau/pydantic never does (``0.00005``) — a real quirk the Rust port must match,
    not a defect in the fixture.
    """
    obj = json.loads(raw_json)
    reparsed = json.loads(json.dumps(obj, ensure_ascii=False))
    assert _key_sequence(obj) == _key_sequence(reparsed), (
        f"key-order round-trip mismatch for {rel}"
    )
    path = fixtures_root() / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(raw_json + "\n", encoding="utf-8")
    return path


def compact(obj: Any) -> str:
    """json.dumps in tau's compact separators (matches pydantic's byte layout)."""
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
