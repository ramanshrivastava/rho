"""Extract Tau-v1 legacy-migration fixtures (fixtures/wire-legacy/).

Each case is a hand-crafted *legacy* session JSONL line (as an old Tau-v1 client
would have persisted it) paired with a ``.expected.json`` — the canonical entry
tau's ``_migrate_message`` + re-serialization produces. Together they pin every
branch of the migration in ``tau_agent/session/jsonl.py`` so the Rust port can
replay real user history without breakage.

Branches covered:
  * role "tool"      -> toolResult (name->toolName, tool_call_id->toolCallId,
                        ok->isError, string content->blocks, data+details merge,
                        error->content when content empty)
  * role "assistant" -> usage.cost null -> {}; string content + tool_calls;
                        list content + tool_calls; camelCase toolCalls alias
  * role "user"      -> custom (custom_type / customType -> custom role)
"""

from __future__ import annotations

import json

from _common import patch_determinism, write_text

patch_determinism()

from tau_agent.session.jsonl import entry_from_json_line, entry_to_json_line  # noqa: E402

# name -> legacy entry (as a Python dict we serialize to the raw .jsonl input)
LEGACY_CASES: dict[str, dict] = {
    # role "tool": ok=true, string content, data + details merged.
    "tool_ok_data_details": {
        "type": "message", "id": "L1", "parent_id": None, "timestamp": 1731234567.0,
        "message": {
            "role": "tool", "name": "read_file", "tool_call_id": "c1", "ok": True,
            "content": "file body", "data": {"path": "README.md"},
            "details": {"extra": 1}, "timestamp": 1731234567890,
        },
    },
    # role "tool": ok=false, empty content, error text becomes the content.
    "tool_error_empty_content": {
        "type": "message", "id": "L2", "parent_id": "L1", "timestamp": 1731234567.0,
        "message": {
            "role": "tool", "name": "bash", "tool_call_id": "c2", "ok": False,
            "content": "", "error": "command failed", "timestamp": 1731234567890,
        },
    },
    # role "tool": data present, details absent -> details = data.
    "tool_data_only": {
        "type": "message", "id": "L3", "parent_id": "L2", "timestamp": 1731234567.0,
        "message": {
            "role": "tool", "name": "grep", "tool_call_id": "c3", "ok": True,
            "content": "match", "data": {"count": 3}, "timestamp": 1731234567890,
        },
    },
    # role "assistant": string content + tool_calls, usage.cost null -> {}.
    "assistant_string_content_tool_calls": {
        "type": "message", "id": "L4", "parent_id": "L3", "timestamp": 1731234567.0,
        "message": {
            "role": "assistant", "content": "I'll read it",
            "tool_calls": [
                {"type": "toolCall", "id": "c1", "name": "read_file",
                 "arguments": {"path": "x"}}
            ],
            "model": "legacy-model",
            "usage": {"input": 1, "output": 2, "cost": None},
            "timestamp": 1731234567890,
        },
    },
    # role "assistant": list content + tool_calls appended.
    "assistant_list_content_tool_calls": {
        "type": "message", "id": "L5", "parent_id": "L4", "timestamp": 1731234567.0,
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "sure"}],
            "tool_calls": [
                {"type": "toolCall", "id": "c2", "name": "write", "arguments": {}}
            ],
            "model": "legacy-model", "timestamp": 1731234567890,
        },
    },
    # role "assistant": camelCase toolCalls alias + string content.
    "assistant_camel_toolcalls": {
        "type": "message", "id": "L6", "parent_id": "L5", "timestamp": 1731234567.0,
        "message": {
            "role": "assistant", "content": "calling",
            "toolCalls": [
                {"type": "toolCall", "id": "c3", "name": "ls", "arguments": {}}
            ],
            "model": "legacy-model", "timestamp": 1731234567890,
        },
    },
    # role "assistant": usage.cost null only (no tool calls).
    "assistant_usage_cost_null": {
        "type": "message", "id": "L7", "parent_id": "L6", "timestamp": 1731234567.0,
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "done"}],
            "usage": {"input": 5, "output": 3, "cost": None},
            "model": "legacy-model", "timestamp": 1731234567890,
        },
    },
    # role "user" + snake_case custom_type -> custom role.
    "user_custom_type_snake": {
        "type": "message", "id": "L8", "parent_id": "L7", "timestamp": 1731234567.0,
        "message": {
            "role": "user", "content": "remember this", "custom_type": "reminder",
            "timestamp": 1731234567890,
        },
    },
    # role "user" + camelCase customType -> custom role.
    "user_custom_type_camel": {
        "type": "message", "id": "L9", "parent_id": "L8", "timestamp": 1731234567.0,
        "message": {
            "role": "user", "content": "a tip", "customType": "tip",
            "display": False, "timestamp": 1731234567890,
        },
    },
    # --- Regression cases (PR #2 review): validators + Python truthiness -------
    # role "assistant": usage is *null* -> Usage() (mode="before" validator, not
    # just the migration). Pins usage:null vs a merely-absent key.
    "assistant_usage_null": {
        "type": "message", "id": "R1", "parent_id": None, "timestamp": 1731234567.0,
        "message": {
            "role": "assistant", "content": [{"type": "text", "text": "hi"}],
            "usage": None, "model": "m", "timestamp": 1731234567890,
        },
    },
    # role "tool": empty-string error is *falsy* -> ignored (content stays []).
    "tool_error_empty_string": {
        "type": "message", "id": "R2", "parent_id": "R1", "timestamp": 1731234567.0,
        "message": {
            "role": "tool", "name": "bash", "tool_call_id": "c", "ok": True,
            "content": "", "error": "", "timestamp": 1731234567890,
        },
    },
    # role "tool": falsy ok (0) -> not bool(0) -> isError:true.
    "tool_ok_falsy_zero": {
        "type": "message", "id": "R3", "parent_id": "R2", "timestamp": 1731234567.0,
        "message": {
            "role": "tool", "name": "bash", "tool_call_id": "c", "ok": 0,
            "content": "out", "timestamp": 1731234567890,
        },
    },
    # role "assistant": bare string content -> single text block (validator).
    "assistant_string_content_only": {
        "type": "message", "id": "R4", "parent_id": "R3", "timestamp": 1731234567.0,
        "message": {
            "role": "assistant", "content": "just text", "model": "m",
            "timestamp": 1731234567890,
        },
    },
}


def extract() -> int:
    n = 0
    for name, legacy_entry in LEGACY_CASES.items():
        raw_line = json.dumps(legacy_entry, ensure_ascii=False, separators=(",", ":"))
        write_text(f"wire-legacy/{name}.jsonl", raw_line + "\n")
        migrated = entry_from_json_line(raw_line)
        write_text(f"wire-legacy/{name}.expected.json",
                   entry_to_json_line(migrated).rstrip("\n") + "\n")
        n += 1
    return n


if __name__ == "__main__":
    print(f"wire-legacy: wrote {extract()} case pairs")
