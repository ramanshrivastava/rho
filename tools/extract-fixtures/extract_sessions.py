"""Extract real + synthetic session JSONL files (fixtures/sessions/).

Hand-authored sessions are written through tau's ``JsonlSessionStorage`` (the real
append path -> ``entry_to_json_line``) so the bytes are exactly what tau persists:

  * ``linear.jsonl``       — a linear multi-turn conversation.
  * ``branched.jsonl``     — a branching tree with two leaves + a branch summary.
  * ``compaction.jsonl``   — a conversation with a compaction entry.
  * ``kitchen-sink.jsonl`` — session_info, label, model_change,
                             thinking_level_change, custom, leaf + messages.
  * ``legacy-v1.jsonl``    — a hand-crafted Tau-v1 file (mixed legacy + modern),
                             with ``legacy-v1.expected.jsonl`` = what tau reads it
                             back as and re-serializes.

Synthetic trees under ``sessions/synthetic/`` (1k / 10k / 100k entries, in
``linear`` / ``deep-branch`` / ``compaction-heavy`` shapes) feed the M6
benchmarks; the 100k files are gzipped.
"""

from __future__ import annotations

import asyncio
import gzip
import json

from _common import fixtures_root, patch_determinism, write_text

patch_determinism()

from tau_agent.messages import (  # noqa: E402
    AssistantMessage,
    TextContent,
    ToolCall,
    ToolResultMessage,
    UserMessage,
)
from tau_agent.session.entries import (  # noqa: E402
    BranchSummaryEntry,
    CompactionEntry,
    CustomEntry,
    LabelEntry,
    LeafEntry,
    MessageEntry,
    ModelChangeEntry,
    SessionInfoEntry,
    ThinkingLevelChangeEntry,
)
from tau_agent.session.jsonl import entries_from_json_lines, entry_to_json_line  # noqa: E402
from tau_agent.session.storage import JsonlSessionStorage  # noqa: E402

BASE_TS = 1_731_234_567.0
BASE_MS = 1_731_234_567_000


def _ts(i: int) -> float:
    return BASE_TS + i


async def _write_session(rel: str, entries: list) -> None:
    path = fixtures_root() / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    storage = JsonlSessionStorage(path)
    for entry in entries:
        await storage.append(entry)


def _linear_entries() -> list:
    return [
        SessionInfoEntry(id="s0", parent_id=None, timestamp=_ts(0),
                         created_at=_ts(0), cwd="/work", title="Linear demo"),
        MessageEntry(id="s1", parent_id="s0", timestamp=_ts(1),
                     message=UserMessage(content="What is 2+2?", timestamp=BASE_MS)),
        MessageEntry(id="s2", parent_id="s1", timestamp=_ts(2),
                     message=AssistantMessage(
                         content=[TextContent(text="4")], model="fake",
                         timestamp=BASE_MS)),
        MessageEntry(id="s3", parent_id="s2", timestamp=_ts(3),
                     message=UserMessage(content="And 3+3?", timestamp=BASE_MS)),
        MessageEntry(id="s4", parent_id="s3", timestamp=_ts(4),
                     message=AssistantMessage(
                         content=[TextContent(text="6")], model="fake",
                         timestamp=BASE_MS)),
        LeafEntry(id="s5", parent_id="s4", timestamp=_ts(5), entry_id="s4"),
    ]


def _branched_entries() -> list:
    # s2 has two children (s3a and s3b) -> two branches; a branch summary records
    # returning from branch A, and the leaf points at branch B's tip.
    return [
        MessageEntry(id="b0", parent_id=None, timestamp=_ts(0),
                     message=UserMessage(content="Pick a path", timestamp=BASE_MS)),
        MessageEntry(id="b1", parent_id="b0", timestamp=_ts(1),
                     message=AssistantMessage(content=[TextContent(text="left or right?")],
                                              model="fake", timestamp=BASE_MS)),
        MessageEntry(id="b2a", parent_id="b1", timestamp=_ts(2),
                     message=UserMessage(content="left", timestamp=BASE_MS)),
        MessageEntry(id="b3a", parent_id="b2a", timestamp=_ts(3),
                     message=AssistantMessage(content=[TextContent(text="went left")],
                                              model="fake", timestamp=BASE_MS)),
        # second branch off b1
        MessageEntry(id="b2b", parent_id="b1", timestamp=_ts(4),
                     message=UserMessage(content="right", timestamp=BASE_MS)),
        MessageEntry(id="b3b", parent_id="b2b", timestamp=_ts(5),
                     message=AssistantMessage(content=[TextContent(text="went right")],
                                              model="fake", timestamp=BASE_MS)),
        BranchSummaryEntry(id="b4", parent_id="b3b", timestamp=_ts(6),
                           summary="explored the left branch", branch_root_id="b2a"),
        LeafEntry(id="b5", parent_id="b4", timestamp=_ts(7), entry_id="b4"),
    ]


def _compaction_entries() -> list:
    tool_call = ToolCall(id="tc1", name="echo", arguments={"x": 1})
    return [
        MessageEntry(id="c0", parent_id=None, timestamp=_ts(0),
                     message=UserMessage(content="do a lot", timestamp=BASE_MS)),
        MessageEntry(id="c1", parent_id="c0", timestamp=_ts(1),
                     message=AssistantMessage(content=[tool_call], model="fake",
                                              stop_reason="toolUse", timestamp=BASE_MS)),
        MessageEntry(id="c2", parent_id="c1", timestamp=_ts(2),
                     message=ToolResultMessage(tool_call_id="tc1", tool_name="echo",
                                               content=[TextContent(text="ok")],
                                               timestamp=BASE_MS)),
        MessageEntry(id="c3", parent_id="c2", timestamp=_ts(3),
                     message=AssistantMessage(content=[TextContent(text="all done")],
                                              model="fake", timestamp=BASE_MS)),
        CompactionEntry(id="c4", parent_id="c3", timestamp=_ts(4),
                        summary="user asked for a lot; tools ran; done",
                        replaces_entry_ids=["c0", "c1", "c2", "c3"]),
        MessageEntry(id="c5", parent_id="c4", timestamp=_ts(5),
                     message=UserMessage(content="continue", timestamp=BASE_MS)),
        LeafEntry(id="c6", parent_id="c5", timestamp=_ts(6), entry_id="c5"),
    ]


def _kitchen_sink_entries() -> list:
    return [
        SessionInfoEntry(id="k0", parent_id=None, timestamp=_ts(0),
                         created_at=_ts(0), cwd="/work", title="Everything"),
        LabelEntry(id="k1", parent_id="k0", timestamp=_ts(1), label="My Session"),
        ModelChangeEntry(id="k2", parent_id="k1", timestamp=_ts(2),
                         model="claude-sonnet"),
        ThinkingLevelChangeEntry(id="k3", parent_id="k2", timestamp=_ts(3),
                                 thinking_level="high"),
        MessageEntry(id="k4", parent_id="k3", timestamp=_ts(4),
                     message=UserMessage(content="hi 🌍", timestamp=BASE_MS)),
        MessageEntry(id="k5", parent_id="k4", timestamp=_ts(5),
                     message=AssistantMessage(content=[TextContent(text="hello 世界")],
                                              model="claude-sonnet", timestamp=BASE_MS)),
        CustomEntry(id="k6", parent_id="k5", timestamp=_ts(6), namespace="ext.todo",
                    data={"items": ["a", "b"], "done": False}),
        ThinkingLevelChangeEntry(id="k7", parent_id="k6", timestamp=_ts(7),
                                 thinking_level=None),
        LeafEntry(id="k8", parent_id="k7", timestamp=_ts(8), entry_id="k5"),
    ]


def _legacy_v1_lines() -> list[str]:
    # A Tau-v1 file mixing legacy message shapes with modern entries.
    entries = [
        {"type": "message", "id": "v0", "parent_id": None, "timestamp": BASE_TS,
         "message": {"role": "user", "content": "start", "timestamp": BASE_MS}},
        {"type": "message", "id": "v1", "parent_id": "v0", "timestamp": BASE_TS,
         "message": {"role": "assistant", "content": "on it",
                     "tool_calls": [{"type": "toolCall", "id": "t1", "name": "read",
                                     "arguments": {"path": "a"}}],
                     "usage": {"input": 3, "output": 4, "cost": None},
                     "model": "old", "timestamp": BASE_MS}},
        {"type": "message", "id": "v2", "parent_id": "v1", "timestamp": BASE_TS,
         "message": {"role": "tool", "name": "read", "tool_call_id": "t1", "ok": True,
                     "content": "contents", "data": {"path": "a"},
                     "timestamp": BASE_MS}},
        {"type": "message", "id": "v3", "parent_id": "v2", "timestamp": BASE_TS,
         "message": {"role": "user", "content": "note to self",
                     "custom_type": "reminder", "timestamp": BASE_MS}},
        {"type": "label", "id": "v4", "parent_id": "v3", "timestamp": BASE_TS,
         "label": "legacy session"},
    ]
    return [json.dumps(e, ensure_ascii=False, separators=(",", ":")) for e in entries]


# --- synthetic generators (return raw JSONL text) ----------------------------


def _synthetic_linear(n: int) -> str:
    lines = []
    for i in range(n):
        parent = None if i == 0 else f"e{i - 1}"
        role_user = i % 2 == 0
        msg = (UserMessage(content=f"msg {i}", timestamp=BASE_MS) if role_user
               else AssistantMessage(content=[TextContent(text=f"reply {i}")],
                                     model="fake", timestamp=BASE_MS))
        entry = MessageEntry(id=f"e{i}", parent_id=parent, timestamp=_ts(i), message=msg)
        lines.append(entry_to_json_line(entry).rstrip("\n"))
    return "\n".join(lines) + "\n"


def _synthetic_deep_branch(n: int) -> str:
    # A bushy tree: every 4th node branches off an earlier node instead of the
    # immediate parent, producing many leaves.
    lines = []
    for i in range(n):
        if i == 0:
            parent = None
        elif i % 4 == 0:
            parent = f"e{max(0, i - 3)}"
        else:
            parent = f"e{i - 1}"
        entry = MessageEntry(
            id=f"e{i}", parent_id=parent, timestamp=_ts(i),
            message=UserMessage(content=f"node {i}", timestamp=BASE_MS))
        lines.append(entry_to_json_line(entry).rstrip("\n"))
    return "\n".join(lines) + "\n"


def _synthetic_compaction_heavy(n: int) -> str:
    # Alternating message / compaction entries; each compaction replaces the
    # previous message run.
    lines = []
    prev = None
    for i in range(n):
        if i % 2 == 1 and i > 0:
            entry = CompactionEntry(id=f"e{i}", parent_id=prev, timestamp=_ts(i),
                                    summary=f"compacted up to {i}",
                                    replaces_entry_ids=[f"e{i - 1}"])
        else:
            entry = MessageEntry(id=f"e{i}", parent_id=prev, timestamp=_ts(i),
                                 message=UserMessage(content=f"turn {i}",
                                                     timestamp=BASE_MS))
        lines.append(entry_to_json_line(entry).rstrip("\n"))
        prev = f"e{i}"
    return "\n".join(lines) + "\n"


def _write_synthetic(shape: str, gen) -> int:
    written = 0
    for size, label in [(1_000, "1k"), (10_000, "10k"), (100_000, "100k")]:
        text = gen(size)
        rel = f"sessions/synthetic/{shape}-{label}.jsonl"
        if size >= 100_000:
            path = fixtures_root() / (rel + ".gz")
            path.parent.mkdir(parents=True, exist_ok=True)
            with gzip.GzipFile(filename=path, mode="wb", mtime=0) as gz:
                gz.write(text.encode("utf-8"))
        else:
            write_text(rel, text)
        written += 1
    return written


async def _main() -> int:
    await _write_session("sessions/linear.jsonl", _linear_entries())
    await _write_session("sessions/branched.jsonl", _branched_entries())
    await _write_session("sessions/compaction.jsonl", _compaction_entries())
    await _write_session("sessions/kitchen-sink.jsonl", _kitchen_sink_entries())

    legacy_lines = _legacy_v1_lines()
    write_text("sessions/legacy-v1.jsonl", "\n".join(legacy_lines) + "\n")
    migrated = entries_from_json_lines(legacy_lines)
    write_text("sessions/legacy-v1.expected.jsonl",
               "".join(entry_to_json_line(e) for e in migrated))

    count = 5
    count += _write_synthetic("linear", _synthetic_linear)
    count += _write_synthetic("deep-branch", _synthetic_deep_branch)
    count += _write_synthetic("compaction-heavy", _synthetic_compaction_heavy)
    return count


def extract() -> int:
    return asyncio.run(_main())


if __name__ == "__main__":
    print(f"sessions: wrote {extract()} files")
