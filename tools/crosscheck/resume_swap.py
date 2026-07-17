"""Resume-swap, rho -> tau direction.

The committed ``tools/crosscheck/sessions/<name>.session.jsonl`` files are
**byte-identical** to what rho's ``CodingSession`` writes (verified in
``crates/rho-coding/tests/crosscheck_v2.rs`` by a raw byte-diff). This script
closes the loop: it loads each such (effectively rho-written) file through
*tau's* ``CodingSession`` and asserts tau replays it to the exact same
transcript ``(role, text)`` recorded in ``expected/v2/<name>.state.jsonl``.

Together with the Rust side's tau -> rho resume-swap, this proves sessions are
resumable in both directions across the two implementations.

Run via ``just crosscheck`` (or the ``#[ignore]``d
``crosscheck_v2_resume_swap_rho_to_tau`` test). Requires ``uv`` + a tau checkout.
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

from tau_coding.session import (
    CodingSession,
    CodingSessionConfig,
    jsonl_session_storage,
)

FIXED_CWD = "/rho-crosscheck-cwd"
SCENARIOS = ("text", "tool", "compaction", "branch")


async def _replay_state(session_path: Path) -> list[str]:
    session = await CodingSession.load(CodingSessionConfig(
        provider_settings=None,
        provider=_null_provider(),
        model="fake",
        cwd=Path(FIXED_CWD),
        storage=jsonl_session_storage(session_path),
        provider_name="fake",
        tools=[],
    ))
    return [_canonical_message(m) for m in session.messages]


def _neutralize_timestamps(value) -> None:
    if isinstance(value, dict):
        for key in value:
            if key in ("timestamp", "createdAt", "created_at"):
                value[key] = 0
            else:
                _neutralize_timestamps(value[key])
    elif isinstance(value, list):
        for item in value:
            _neutralize_timestamps(item)


def _canonical_message(message) -> str:
    """Full canonical message JSON (tau's wire serialization), timestamp neutralized."""
    obj = json.loads(message.model_dump_json(by_alias=True, exclude_none=True))
    _neutralize_timestamps(obj)
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def _null_provider():
    from tau_ai import FakeProvider

    return FakeProvider([])


async def _main() -> int:
    here = Path(__file__).resolve().parent
    sessions_dir = here / "sessions"
    v2_dir = here / "expected" / "v2"
    failures: list[str] = []

    for name in SCENARIOS:
        session_path = sessions_dir / f"{name}.session.jsonl"
        expected = (v2_dir / f"{name}.state.jsonl").read_text(encoding="utf-8").splitlines()
        got = await _replay_state(session_path)
        if got != expected:
            failures.append(
                f"{name}: rho->tau resume-swap state diverged\n  got:  {got}\n  want: {expected}"
            )
        else:
            print(f"crosscheck[resume-swap rho->tau]: {name} OK ({len(got)} msgs)")

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("crosscheck[resume-swap rho->tau]: all scenarios OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(_main()))
