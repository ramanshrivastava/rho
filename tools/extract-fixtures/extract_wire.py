"""Extract per-variant wire fixtures (fixtures/wire/).

One JSON file per message role, content block, usage shape, session entry type,
and event variant — each serialized by tau's own code path:

* ``WireModel`` types -> ``model_dump_json(by_alias=True, exclude_none=True)``
  (camelCase aliases, ``None`` fields omitted, int-ms timestamps).
* session entries -> ``entry_to_json_line`` (snake_case top level, ``exclude_none``,
  camelCase *inside* the embedded ``message`` — the mixed-case-per-line quirk).
"""

from __future__ import annotations

from _common import patch_determinism, write_wire_json

patch_determinism()

from tau_agent.events import (  # noqa: E402
    AgentEndEvent,
    AgentStartEvent,
    MessageEndEvent,
    MessageStartEvent,
    MessageUpdateEvent,
    ToolExecutionEndEvent,
    ToolExecutionStartEvent,
    ToolExecutionUpdateEvent,
    TurnEndEvent,
    TurnStartEvent,
)
from tau_agent.messages import (  # noqa: E402
    AssistantDiagnosticError,
    AssistantMessage,
    AssistantMessageDiagnostic,
    BashExecutionMessage,
    BranchSummaryMessage,
    CompactionSummaryMessage,
    CustomMessage,
    ImageContent,
    TextContent,
    ThinkingContent,
    ToolCall,
    ToolResultMessage,
    Usage,
    UsageCost,
    UserMessage,
)
from tau_agent.provider_events import (  # noqa: E402
    AssistantDoneEvent,
    AssistantErrorEvent,
    AssistantStartEvent,
    TextDeltaEvent,
    TextEndEvent,
    TextStartEvent,
    ThinkingDeltaEvent,
    ThinkingEndEvent,
    ThinkingStartEvent,
    ToolCallDeltaEvent,
    ToolCallEndEvent,
    ToolCallStartEvent,
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
from tau_agent.session.jsonl import entry_to_json_line  # noqa: E402
from tau_agent.tools import AgentToolResult  # noqa: E402
from tau_coding.events import (  # noqa: E402
    AgentSettledEvent,
    AutoRetryEndEvent,
    AutoRetryStartEvent,
    CompactionEndEvent,
    CompactionStartEvent,
    EntryAppendedEvent,
    QueueUpdateEvent,
    SessionAgentEndEvent,
    SessionInfoChangedEvent,
    ThinkingLevelChangedEvent,
)

# Reused sample values ---------------------------------------------------------

# Nested objects/arrays + non-ASCII (and emoji) keys, exercising key-order
# preservation and non-ASCII key serialization.
NESTED_ARGS = {
    "café": {"naïve": [1, 2, {"δ": "x"}], "nested": {"a": True, "b": None}},
    "emoji_🔑": "值",
    "list": [{"k": 1}, {"k": 2}],
}
CJK_EMOJI = "こんにちは 🌍 世界 — café"


def _wire(rel: str, model: object) -> None:
    write_wire_json(rel, model.model_dump_json(by_alias=True, exclude_none=True))


def _entry(rel: str, entry: object) -> None:
    write_wire_json(rel, entry_to_json_line(entry).rstrip("\n"))


def extract() -> int:
    n = 0

    # --- Content blocks (all 4 types) ---------------------------------------
    _wire("wire/content/text.json", TextContent(text="Hello, world"))
    _wire("wire/content/text_with_signature.json",
          TextContent(text="signed", text_signature="sig-abc"))
    _wire("wire/content/text_unicode.json", TextContent(text=CJK_EMOJI))
    _wire("wire/content/thinking.json", ThinkingContent(thinking="let me think"))
    _wire("wire/content/thinking_signed_redacted.json",
          ThinkingContent(thinking="secret", thinking_signature="tsig", redacted=True))
    _wire("wire/content/image.json",
          ImageContent(data="iVBORw0KGgoAAAANS=", mime_type="image/png"))
    _wire("wire/content/tool_call.json",
          ToolCall(id="call-1", name="read_file", arguments={"path": "README.md"}))
    _wire("wire/content/tool_call_nested_args.json",
          ToolCall(id="call-2", name="complex", arguments=NESTED_ARGS,
                   thought_signature="thought-sig"))
    _wire("wire/content/tool_call_empty_args.json",
          ToolCall(id="call-3", name="noargs"))
    n += 9

    # --- Usage / UsageCost ---------------------------------------------------
    _wire("wire/usage/usage_default.json", Usage())
    _wire("wire/usage/usage_full.json",
          Usage(input=100, output=50, cache_read=10, cache_write=5,
                cache_write_1h=2, reasoning=7, total_tokens=165,
                cost=UsageCost(input=0.001, output=0.002, cache_read=0.0001,
                               cache_write=0.00005, total=0.00325)))
    _wire("wire/usage/usage_cost_default.json", UsageCost())
    _wire("wire/usage/usage_cost_custom.json",
          UsageCost(input=1.5, output=2.25, total=3.75))
    n += 4

    # --- Messages (all 7 roles) ---------------------------------------------
    # user: plain string vs blocks vs empty list vs unicode
    _wire("wire/messages/user_string.json",
          UserMessage(content="Read README.md", timestamp=1731234567890))
    _wire("wire/messages/user_blocks.json",
          UserMessage(content=[TextContent(text="look at this"),
                               ImageContent(data="AAAA", mime_type="image/jpeg")],
                      timestamp=1731234567890))
    _wire("wire/messages/user_empty_blocks.json",
          UserMessage(content=[], timestamp=1731234567890))
    _wire("wire/messages/user_unicode.json",
          UserMessage(content=CJK_EMOJI, timestamp=1731234567890))
    # assistant: default (None-field omission), full multi-block, empty content,
    # with diagnostics.
    _wire("wire/messages/assistant_default.json",
          AssistantMessage(content=[TextContent(text="hi")], model="fake-model"))
    _wire("wire/messages/assistant_multiblock.json",
          AssistantMessage(
              content=[ThinkingContent(thinking="reasoning"),
                       TextContent(text="Here you go"),
                       ToolCall(id="c1", name="do", arguments={"x": [1, 2]})],
              api="anthropic", provider="anthropic", model="claude-x",
              response_model="claude-x-2", response_id="resp_1",
              usage=Usage(input=5, output=3, total_tokens=8),
              stop_reason="toolUse"))
    _wire("wire/messages/assistant_empty_content.json",
          AssistantMessage(content=[], model="m", stop_reason="stop"))
    _wire("wire/messages/assistant_error.json",
          AssistantMessage(content=[], model="m", stop_reason="error",
                           error_message="boom"))
    _wire("wire/messages/assistant_with_diagnostics.json",
          AssistantMessage(
              content=[TextContent(text="ok")], model="m",
              diagnostics=[AssistantMessageDiagnostic(
                  type="retry", timestamp=1731234567890,
                  error=AssistantDiagnosticError(name="HTTPError", message="429",
                                                 code=429),
                  details={"attempt": 1})]))
    # toolResult
    _wire("wire/messages/tool_result_text.json",
          ToolResultMessage(tool_call_id="c1", tool_name="read_file",
                            content=[TextContent(text="file body")],
                            timestamp=1731234567890))
    _wire("wire/messages/tool_result_error_details.json",
          ToolResultMessage(tool_call_id="c2", tool_name="bash",
                            content=[TextContent(text="stderr"),
                                     ImageContent(data="Z", mime_type="image/png")],
                            details={"exit": 1, "keys": {"δ": 1}},
                            added_tool_names=["extra_tool"], is_error=True,
                            timestamp=1731234567890))
    _wire("wire/messages/tool_result_empty.json",
          ToolResultMessage(tool_call_id="c3", tool_name="noop", content=[],
                            timestamp=1731234567890))
    # bashExecution
    _wire("wire/messages/bash_execution.json",
          BashExecutionMessage(command="ls -la", output="total 0\n",
                               exit_code=0, timestamp=1731234567890))
    _wire("wire/messages/bash_execution_cancelled.json",
          BashExecutionMessage(command="sleep 100", output="", exit_code=None,
                               cancelled=True, truncated=True,
                               full_output_path="/tmp/out.txt",
                               exclude_from_context=True,
                               timestamp=1731234567890))
    # custom
    _wire("wire/messages/custom.json",
          CustomMessage(custom_type="note", content="a note",
                        details={"pinned": True}, timestamp=1731234567890))
    _wire("wire/messages/custom_hidden.json",
          CustomMessage(custom_type="internal", content=[TextContent(text="x")],
                        display=False, timestamp=1731234567890))
    # branchSummary / compactionSummary
    _wire("wire/messages/branch_summary.json",
          BranchSummaryMessage(summary="did some work", from_id="entry-9",
                               timestamp=1731234567890))
    _wire("wire/messages/compaction_summary.json",
          CompactionSummaryMessage(summary="earlier turns", tokens_before=4096,
                                   timestamp=1731234567890))
    n += 18

    # --- AgentToolResult -----------------------------------------------------
    _wire("wire/tool_result/agent_tool_result.json",
          AgentToolResult(content=[TextContent(text="done")], details={"ok": True}))
    _wire("wire/tool_result/agent_tool_result_terminate.json",
          AgentToolResult(content=[TextContent(text="bye")],
                          added_tool_names=["t"], terminate=True))
    n += 2

    # --- Session entries (all 9 types) --------------------------------------
    # Note the mixed casing: snake_case top-level fields, camelCase *message*.
    _entry("wire/entries/message_entry.json",
           MessageEntry(id="e1", parent_id=None, timestamp=1731234567.0,
                        message=UserMessage(content="hello", timestamp=1731234567890)))
    _entry("wire/entries/message_entry_assistant.json",
           MessageEntry(id="e2", parent_id="e1", timestamp=1731234567.0,
                        message=AssistantMessage(content=[TextContent(text="hi")],
                                                 model="m")))
    _entry("wire/entries/model_change.json",
           ModelChangeEntry(id="e3", parent_id="e2", timestamp=1731234567.0,
                            model="claude-sonnet"))
    _entry("wire/entries/thinking_level_change.json",
           ThinkingLevelChangeEntry(id="e4", parent_id="e3", timestamp=1731234567.0,
                                    thinking_level="high"))
    _entry("wire/entries/thinking_level_change_null.json",
           ThinkingLevelChangeEntry(id="e4b", parent_id="e3", timestamp=1731234567.0,
                                    thinking_level=None))
    _entry("wire/entries/compaction.json",
           CompactionEntry(id="e5", parent_id="e4", timestamp=1731234567.0,
                           summary="summary text",
                           replaces_entry_ids=["e1", "e2"]))
    _entry("wire/entries/branch_summary.json",
           BranchSummaryEntry(id="e6", parent_id="e5", timestamp=1731234567.0,
                              summary="branch summary", branch_root_id="e2"))
    _entry("wire/entries/label.json",
           LabelEntry(id="e7", parent_id="e6", timestamp=1731234567.0,
                      label="my session"))
    _entry("wire/entries/leaf.json",
           LeafEntry(id="e8", parent_id="e7", timestamp=1731234567.0, entry_id="e6"))
    _entry("wire/entries/session_info.json",
           SessionInfoEntry(id="e9", parent_id=None, timestamp=1731234567.0,
                            created_at=1731234567.0, cwd="/work", title="Demo"))
    _entry("wire/entries/custom_entry.json",
           CustomEntry(id="e10", parent_id="e9", timestamp=1731234567.0,
                       namespace="ext.todo",
                       data={"items": [1, 2], "meta": {"δ": "x"}}))
    n += 11

    # --- AgentEvent types (all 10) ------------------------------------------
    a_msg = AssistantMessage(content=[TextContent(text="hi")], model="fake")
    tool_result_msg = ToolResultMessage(tool_call_id="c1", tool_name="t",
                                         content=[TextContent(text="r")],
                                         timestamp=1731234567890)
    ame = TextDeltaEvent(content_index=0, delta="hi", partial=a_msg)
    _wire("wire/agent-events/agent_start.json", AgentStartEvent())
    _wire("wire/agent-events/agent_end.json", AgentEndEvent(messages=[a_msg]))
    _wire("wire/agent-events/turn_start.json", TurnStartEvent())
    _wire("wire/agent-events/turn_end.json",
          TurnEndEvent(message=a_msg, tool_results=[tool_result_msg]))
    _wire("wire/agent-events/message_start.json", MessageStartEvent(message=a_msg))
    _wire("wire/agent-events/message_update.json",
          MessageUpdateEvent(message=a_msg, assistant_message_event=ame))
    _wire("wire/agent-events/message_end.json", MessageEndEvent(message=a_msg))
    _wire("wire/agent-events/tool_execution_start.json",
          ToolExecutionStartEvent(tool_call_id="c1", tool_name="t",
                                  args={"a": 1}))
    _wire("wire/agent-events/tool_execution_update.json",
          ToolExecutionUpdateEvent(tool_call_id="c1", tool_name="t", args={"a": 1},
                                   partial_result=AgentToolResult(
                                       content=[TextContent(text="partial")])))
    _wire("wire/agent-events/tool_execution_end.json",
          ToolExecutionEndEvent(tool_call_id="c1", tool_name="t",
                                result=AgentToolResult(
                                    content=[TextContent(text="final")],
                                    details={}),
                                is_error=False))
    n += 10

    # --- AssistantMessageEvent types (all 12) -------------------------------
    partial = AssistantMessage(content=[TextContent(text="hi")], model="fake")
    _wire("wire/assistant-events/start.json",
          AssistantStartEvent(partial=AssistantMessage(model="fake")))
    _wire("wire/assistant-events/text_start.json",
          TextStartEvent(content_index=0, partial=partial))
    _wire("wire/assistant-events/text_delta.json",
          TextDeltaEvent(content_index=0, delta="hi", partial=partial))
    _wire("wire/assistant-events/text_end.json",
          TextEndEvent(content_index=0, content="hi", partial=partial))
    _wire("wire/assistant-events/thinking_start.json",
          ThinkingStartEvent(content_index=0, partial=partial))
    _wire("wire/assistant-events/thinking_delta.json",
          ThinkingDeltaEvent(content_index=0, delta="think", partial=partial))
    _wire("wire/assistant-events/thinking_end.json",
          ThinkingEndEvent(content_index=0, content="think", partial=partial))
    _wire("wire/assistant-events/toolcall_start.json",
          ToolCallStartEvent(content_index=0, partial=partial))
    _wire("wire/assistant-events/toolcall_delta.json",
          ToolCallDeltaEvent(content_index=0, delta='{"a":', partial=partial))
    _wire("wire/assistant-events/toolcall_end.json",
          ToolCallEndEvent(content_index=0,
                           tool_call=ToolCall(id="c1", name="t", arguments={"a": 1}),
                           partial=partial))
    _wire("wire/assistant-events/done.json",
          AssistantDoneEvent(reason="stop", message=partial))
    _wire("wire/assistant-events/error.json",
          AssistantErrorEvent(reason="error",
                              error=AssistantMessage(model="fake", stop_reason="error",
                                                     error_message="nope")))
    n += 12

    # --- SessionOwnEvent types (coding-layer, all 10) -----------------------
    _wire("wire/session-events/agent_end.json",
          SessionAgentEndEvent(messages=[a_msg], will_retry=False))
    _wire("wire/session-events/agent_settled.json", AgentSettledEvent())
    _wire("wire/session-events/queue_update.json",
          QueueUpdateEvent(steering=("a", "b"), follow_up=("c",)))
    _wire("wire/session-events/compaction_start.json",
          CompactionStartEvent(reason="threshold"))
    _wire("wire/session-events/compaction_end.json",
          CompactionEndEvent(reason="threshold", aborted=False))
    _wire("wire/session-events/entry_appended.json",
          EntryAppendedEvent(entry=LabelEntry(id="e7", parent_id="e6",
                                               timestamp=1731234567.0,
                                               label="lbl")))
    _wire("wire/session-events/session_info_changed.json",
          SessionInfoChangedEvent(name="renamed"))
    _wire("wire/session-events/thinking_level_changed.json",
          ThinkingLevelChangedEvent(level="high"))
    _wire("wire/session-events/auto_retry_start.json",
          AutoRetryStartEvent(attempt=1, max_attempts=3, delay_ms=1000,
                              error_message="429 Too Many Requests"))
    _wire("wire/session-events/auto_retry_end.json",
          AutoRetryEndEvent(success=True, attempt=2, final_error=None))
    n += 10

    return n


if __name__ == "__main__":
    print(f"wire: wrote {extract()} fixtures")
