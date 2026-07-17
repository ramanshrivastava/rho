"""Extract the assembled system-prompt golden (fixtures/system-prompt/).

The system prompt is deterministic given a tool set, date, and cwd. rho must
byte-match tau's `build_system_prompt` for the default coding-tool set, so this
writes tau's exact output for a *fixed* cwd and date; the Rust golden test
(`crates/rho-coding/tests/system_prompt_golden.rs`) rebuilds the prompt with the
same inputs and asserts equality.

The tool prompt snippets/guidelines are cwd-independent (they are static
metadata), so the only cwd-dependent line is the trailing
`Current working directory:` — pinned here to a fixed absolute path.
"""

from __future__ import annotations

from datetime import date
from pathlib import Path

from _common import write_text

# A fixed, platform-neutral cwd string. Only the trailing prompt line depends on
# it; the Rust golden test builds with this exact path + date.
FIXED_CWD = Path("/tmp/rho-fixture-cwd")
FIXED_DATE = date(2026, 6, 17)


def extract() -> int:
    from tau_coding.system_prompt import BuildSystemPromptOptions, build_system_prompt
    from tau_coding.tools import create_coding_tools

    tools = create_coding_tools(cwd=FIXED_CWD)
    prompt = build_system_prompt(
        BuildSystemPromptOptions(
            cwd=FIXED_CWD,
            tools=tools,
            current_date=FIXED_DATE,
        )
    )
    write_text("system-prompt/default_coding_tools.txt", prompt)
    return 1


if __name__ == "__main__":
    print(f"system-prompt: {extract()} fixture(s)")
