"""Extract an HTML session-export golden (fixtures/export/).

Renders the ``kitchen-sink`` session (all entry types) through tau's real
``render_session_html``. The ``generated_at`` timestamp is frozen by
``patch_determinism`` so the HTML is byte-reproducible.
"""

from __future__ import annotations

from _common import fixtures_root, patch_determinism, write_text

patch_determinism()

from tau_agent.session.jsonl import entries_from_json_lines  # noqa: E402
from tau_coding.session_export import render_session_html  # noqa: E402


def extract() -> int:
    src = fixtures_root() / "sessions" / "kitchen-sink.jsonl"
    entries = entries_from_json_lines(src.read_text(encoding="utf-8").splitlines())
    html = render_session_html(entries, title="Kitchen Sink Session",
                               source="fixtures/sessions/kitchen-sink.jsonl")
    write_text("export/kitchen-sink.html", html)
    return 1


if __name__ == "__main__":
    print(f"export: wrote {extract()} file(s)")
