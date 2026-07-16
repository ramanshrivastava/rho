"""Extract provider SSE wire transcripts (fixtures/sse/).

For each provider adapter we drive the *real* tau adapter against a canned SSE
response body (served through an ``httpx.MockTransport``) and capture, per case:

  * ``<case>.request.json`` — the request payload tau built (proves request shape).
  * ``<case>.sse``          — the raw SSE response body fed to the adapter.
  * ``<case>.events.jsonl`` — the canonical ``AssistantMessageEvent`` sequence the
                              adapter produced (``model_dump_json`` per line).

The SSE bodies are taken from / modelled on tau's own provider tests so they are
guaranteed to parse. Providers covered: anthropic, openai_compatible, openai_codex,
google, mistral, and the (HTTP-less) fake provider. Cases exercise text streaming,
tool calls, thinking/reasoning, usage reporting, and error events.
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import Mapping

import httpx

from _common import compact, patch_determinism, write_text

patch_determinism()

from tau_agent.messages import UserMessage  # noqa: E402
from tau_agent.tools import AgentTool, AgentToolResult  # noqa: E402
from tau_ai import (  # noqa: E402
    AnthropicConfig,
    AnthropicProvider,
    FakeProvider,
    GoogleGenerativeAIProvider,
    OpenAICodexConfig,
    OpenAICodexCredentials,
    OpenAICodexProvider,
    OpenAICompatibleConfig,
    OpenAICompatibleProvider,
)
from tau_ai.events import (  # noqa: E402
    AssistantDoneEvent,
    AssistantStartEvent,
    TextDeltaEvent,
)
from tau_ai.mistral import MistralConversationsProvider  # noqa: E402


def _tool(name: str, description: str, params: Mapping) -> AgentTool:
    async def execute(tool_call_id, arguments, signal=None, on_update=None):
        del tool_call_id, signal, on_update
        return AgentToolResult(content=[])

    return AgentTool(name=name, label=name, description=description,
                     parameters=params, execute_fn=execute)


BASH_TOOL = _tool("bash", "Run a shell command",
                  {"type": "object", "properties": {"command": {"type": "string"}}})
READ_TOOL = _tool("read_file", "Read a file",
                  {"type": "object", "properties": {"path": {"type": "string"}}})


async def _run_http_case(provider_factory, *, model, system, messages, tools, sse,
                         provider_name, case) -> None:
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["request"] = request
        return httpx.Response(200 if not case.endswith("error") else 400,
                              text=sse,
                              headers={"content-type": "text/event-stream"})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        provider = provider_factory(client)
        events = [
            e async for e in provider.stream_response(
                model=model, system=system, messages=messages, tools=tools)
        ]

    request = captured["request"]
    try:
        payload = json.loads(request.content)
        write_text(f"sse/{provider_name}/{case}.request.json", compact(payload) + "\n")
    except (json.JSONDecodeError, UnicodeDecodeError):
        pass
    write_text(f"sse/{provider_name}/{case}.sse", sse)
    lines = [e.model_dump_json(by_alias=True, exclude_none=True) for e in events]
    write_text(f"sse/{provider_name}/{case}.events.jsonl", "\n".join(lines) + "\n")


def _cases() -> list[dict]:
    user = [UserMessage(content="Say hello", timestamp=1731234567890)]
    use_tool = [UserMessage(content="run ls", timestamp=1731234567890)]

    return [
        # --- openai_compatible ------------------------------------------------
        {"provider_name": "openai_compatible", "case": "text",
         "factory": lambda c: OpenAICompatibleProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://x.test/v1"), client=c),
         "model": "gpt-x", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"choices":[{"delta":{"content":"Hel"}}]}\n\n'
                 'data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}\n\n'
                 'data: [DONE]\n\n')},
        {"provider_name": "openai_compatible", "case": "reasoning",
         "factory": lambda c: OpenAICompatibleProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://x.test/v1"), client=c),
         "model": "gpt-x", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"choices":[{"delta":{"reasoning_content":"plan "}}]}\n\n'
                 'data: {"choices":[{"delta":{"reasoning_content":"steps"}}]}\n\n'
                 'data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}\n\n'
                 'data: [DONE]\n\n')},
        {"provider_name": "openai_compatible", "case": "tool_calls",
         "factory": lambda c: OpenAICompatibleProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://x.test/v1"), client=c),
         "model": "gpt-x", "system": "You are Tau.", "messages": use_tool,
         "tools": [READ_TOOL],
         "sse": ('data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1",'
                 '"function":{"name":"read_file","arguments":"{\\"path\\":"}}]}}]}\n\n'
                 'data: {"choices":[{"delta":{"tool_calls":[{"index":0,'
                 '"function":{"arguments":"\\"a.txt\\"}"}}]},"finish_reason":"tool_calls"}]}\n\n'
                 'data: [DONE]\n\n')},
        {"provider_name": "openai_compatible", "case": "error",
         "factory": lambda c: OpenAICompatibleProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://x.test/v1",
                                    max_retries=0), client=c),
         "model": "gpt-x", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": '{"error":{"message":"bad request"}}'},

        # --- openai_codex -----------------------------------------------------
        {"provider_name": "openai_codex", "case": "text",
         "factory": lambda c: OpenAICodexProvider(
             OpenAICodexConfig(credential_resolver=_codex_creds,
                               base_url="https://chatgpt.test/backend-api"), client=c),
         "model": "gpt-5.5", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"type":"response.output_text.delta","delta":"Hel"}\n\n'
                 'data: {"type":"response.output_text.delta","delta":"lo"}\n\n'
                 'data: {"type":"response.completed","response":{"status":"completed"}}\n\n')},
        {"provider_name": "openai_codex", "case": "reasoning",
         "factory": lambda c: OpenAICodexProvider(
             OpenAICodexConfig(credential_resolver=_codex_creds,
                               base_url="https://chatgpt.test/backend-api"), client=c),
         "model": "gpt-5.5", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"type":"response.reasoning.delta","delta":"trace "}\n\n'
                 'data: {"type":"response.reasoning_text.delta","delta":"details"}\n\n'
                 'data: {"type":"response.output_text.delta","delta":"Done"}\n\n'
                 'data: {"type":"response.completed","response":{"status":"completed"}}\n\n')},
        {"provider_name": "openai_codex", "case": "tool_calls",
         "factory": lambda c: OpenAICodexProvider(
             OpenAICodexConfig(credential_resolver=_codex_creds,
                               base_url="https://chatgpt.test/backend-api"), client=c),
         "model": "gpt-5.5", "system": "You are Tau.", "messages": use_tool,
         "tools": [READ_TOOL],
         "sse": ('data: {"type":"response.output_item.added","output_index":0,'
                 '"item":{"type":"function_call","id":"fc1","call_id":"call-1",'
                 '"name":"read_file"}}\n\n'
                 'data: {"type":"response.function_call_arguments.delta",'
                 '"delta":"{\\"path\\":"}\n\n'
                 'data: {"type":"response.function_call_arguments.done",'
                 '"arguments":"{\\"path\\":\\"a.txt\\"}"}\n\n'
                 'data: {"type":"response.output_item.done","output_index":0,'
                 '"item":{"type":"function_call","id":"fc1","call_id":"call-1",'
                 '"name":"read_file","arguments":"{\\"path\\":\\"a.txt\\"}"}}\n\n'
                 'data: {"type":"response.completed","response":{"status":"completed"}}\n\n')},

        # --- google -----------------------------------------------------------
        {"provider_name": "google", "case": "text",
         "factory": lambda c: GoogleGenerativeAIProvider(
             OpenAICompatibleConfig(
                 api_key="k",
                 base_url="https://generativelanguage.googleapis.com/v1beta"), client=c),
         "model": "gemini-2.5-flash", "system": "You are Tau.", "messages": user,
         "tools": [],
         "sse": ('data: {"candidates":[{"content":{"parts":[{"text":"ok"}]},'
                 '"finishReason":"STOP"}]}\n\n')},
        {"provider_name": "google", "case": "tool_calls",
         "factory": lambda c: GoogleGenerativeAIProvider(
             OpenAICompatibleConfig(
                 api_key="k",
                 base_url="https://generativelanguage.googleapis.com/v1beta"), client=c),
         "model": "gemini-2.5-flash", "system": "You are Tau.", "messages": use_tool,
         "tools": [BASH_TOOL],
         "sse": ('data: {"candidates":[{"content":{"parts":[{"functionCall":'
                 '{"id":"call-1","name":"bash","args":{"command":"ls"}},'
                 '"thoughtSignature":"sig-123"}]},"finishReason":"STOP"}]}\n\n')},

        # --- anthropic --------------------------------------------------------
        {"provider_name": "anthropic", "case": "text",
         "factory": lambda c: AnthropicProvider(
             AnthropicConfig(api_key="k", base_url="https://api.anthropic.test/v1"),
             client=c),
         "model": "claude-x", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"type":"message_start","message":{"content":[],'
                 '"usage":{"input_tokens":10,"output_tokens":0}}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"text_delta","text":"Hel"}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"text_delta","text":"lo"}}\n\n'
                 'data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},'
                 '"usage":{"output_tokens":5}}\n\n'
                 'data: {"type":"message_stop"}\n\n')},
        {"provider_name": "anthropic", "case": "thinking",
         "factory": lambda c: AnthropicProvider(
             AnthropicConfig(api_key="k", base_url="https://api.anthropic.test/v1"),
             client=c),
         "model": "claude-x", "system": "You are Tau.", "messages": user, "tools": [],
         "sse": ('data: {"type":"message_start","message":{"content":[]}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"thinking_delta","thinking":"pondering"}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"text_delta","text":"answer"}}\n\n'
                 'data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}\n\n'
                 'data: {"type":"message_stop"}\n\n')},
        {"provider_name": "anthropic", "case": "tool_calls",
         "factory": lambda c: AnthropicProvider(
             AnthropicConfig(api_key="k", base_url="https://api.anthropic.test/v1"),
             client=c),
         "model": "claude-x", "system": "You are Tau.", "messages": use_tool,
         "tools": [BASH_TOOL],
         "sse": ('data: {"type":"message_start","message":{"content":[]}}\n\n'
                 'data: {"type":"content_block_start","index":0,'
                 '"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"input_json_delta","partial_json":"{\\"command\\":"}}\n\n'
                 'data: {"type":"content_block_delta","index":0,'
                 '"delta":{"type":"input_json_delta","partial_json":"\\"ls\\"}"}}\n\n'
                 'data: {"type":"content_block_stop","index":0}\n\n'
                 'data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}\n\n'
                 'data: {"type":"message_stop"}\n\n')},

        # --- mistral ----------------------------------------------------------
        {"provider_name": "mistral", "case": "text",
         "factory": lambda c: MistralConversationsProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://api.mistral.test/v1"),
             client=c),
         "model": "mistral-large", "system": "You are Tau.", "messages": user,
         "tools": [],
         "sse": ('data: {"choices":[{"delta":{"content":"Hel"}}]}\n\n'
                 'data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}\n\n'
                 'data: [DONE]\n\n')},
        {"provider_name": "mistral", "case": "tool_calls",
         "factory": lambda c: MistralConversationsProvider(
             OpenAICompatibleConfig(api_key="k", base_url="https://api.mistral.test/v1"),
             client=c),
         "model": "mistral-large", "system": "You are Tau.", "messages": use_tool,
         "tools": [READ_TOOL],
         "sse": ('data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1",'
                 '"function":{"name":"read_file","arguments":"{\\"path\\": \\"a.txt\\"}"}}]},'
                 '"finish_reason":"tool_calls"}]}\n\n'
                 'data: [DONE]\n\n')},
    ]


async def _codex_creds() -> OpenAICodexCredentials:
    return OpenAICodexCredentials(access_token="access-token", account_id="account-1")


async def _run_fake() -> None:
    # The fake provider is HTTP-less: it replays canonical events verbatim. We
    # store the scripted input and the (identical) canonical output so rho's fake
    # provider can be validated against the same replay.
    from tau_agent.messages import AssistantMessage, TextContent

    empty = AssistantMessage(model="fake")
    full = AssistantMessage(model="fake", content=[TextContent(text="hello")])
    script = [
        AssistantStartEvent(partial=empty),
        TextDeltaEvent(content_index=0, delta="hello", partial=full),
        AssistantDoneEvent(reason="stop", message=full),
    ]
    provider = FakeProvider([script])
    events = [
        e async for e in provider.stream_response(
            model="fake", system="You are Tau.",
            messages=[UserMessage(content="hi", timestamp=1731234567890)], tools=[])
    ]
    write_text("sse/fake/text.input.jsonl",
               "\n".join(e.model_dump_json(by_alias=True, exclude_none=True)
                         for e in script) + "\n")
    write_text("sse/fake/text.events.jsonl",
               "\n".join(e.model_dump_json(by_alias=True, exclude_none=True)
                         for e in events) + "\n")


async def _main() -> int:
    cases = _cases()
    for spec in cases:
        await _run_http_case(
            spec["factory"], model=spec["model"], system=spec["system"],
            messages=spec["messages"], tools=spec["tools"], sse=spec["sse"],
            provider_name=spec["provider_name"], case=spec["case"])
    await _run_fake()
    return len(cases) + 1


def extract() -> int:
    return asyncio.run(_main())


if __name__ == "__main__":
    print(f"sse: wrote {extract()} cases")
