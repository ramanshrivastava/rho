"""Run every fixture extractor in order. Invoked by ``just refresh-fixtures``.

Usage:
    uv run --project <tau-checkout> python run_all.py

Each extractor is deterministic (see ``_common.patch_determinism``), so running
this twice must produce byte-identical output under ``rho/fixtures/``.
"""

from __future__ import annotations

import extract_event_streams
import extract_export
import extract_sessions
import extract_sse
import extract_wire
import extract_wire_legacy
from _common import tau_rev


def main() -> None:
    print(f"Extracting golden fixtures from tau @ {tau_rev()}")
    print(f"  wire:          {extract_wire.extract()} fixtures")
    print(f"  wire-legacy:   {extract_wire_legacy.extract()} case pairs")
    print(f"  event-streams: {extract_event_streams.extract()} scenarios")
    print(f"  sessions:      {extract_sessions.extract()} files")
    print(f"  sse:           {extract_sse.extract()} cases")
    print(f"  export:        {extract_export.extract()} file(s)")
    print("done.")


if __name__ == "__main__":
    main()
