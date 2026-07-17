---
title: "Phase 3: Six Providers, One Accumulator, and SSE by Hand"
---

Phase 3 makes `rho-ai` *talk to models*. M2 gave us the provider seam
(`ModelProvider::stream_response`) and drove it with a scripted `FakeProvider`;
M3 fills that seam with the six real adapters — Anthropic, OpenAI-compatible
(chat + responses), OpenAI Codex, Google Gemini, Mistral — plus the shared
HTTP/retry/canonicalization plumbing they stand on, all over `reqwest`. The
correctness bar is again byte-for-byte: the extraction recorded **14 HTTP/SSE
cases** (each with a request-payload JSON, a raw SSE body, and an expected
canonical `AssistantMessageEvent` sequence) plus **one fake-provider replay** (an
event script fed through the HTTP-less `FakeProvider`). Every one of them
reproduces tau's bytes exactly (`tests/request_goldens.rs`, `tests/sse_goldens.rs`).

This note records the decisions a faithful port forced: collapsing tau's
two-layer streaming into one direct-emit accumulator, hand-rolling the SSE
framing, the shared retry envelope with a socket-free test seam, the request
key-order discipline, and the Codex OAuth design.

## The locked decision: canonical events *directly*, no transitional layer

tau streams provider output through **two** hops. Each adapter first emits an
internal `ProviderEvent` union (`tau_ai/_provider_events.py` — `response_start`,
`text_delta`, `thinking_delta`, `tool_call`, `response_end`, `error`, plus a
`retry` progress event), and then `canonicalize_provider_stream`
(`tau_ai/stream.py`) rewrites that stream into the public
`AssistantMessageEvent`s the agent loop consumes. The `_provider_events` layer is
explicitly transitional: its own docstring calls it a "private bridge while [the
parsers] are migrated incrementally," and `canonicalize`'s comment notes the
"public provider protocol exposes only Pi events." It is scaffolding tau never
finished tearing down.

The M3 plan locks in tearing it down. rho adapters drive **one** shared utility —
[`stream::StreamAccumulator`] — which emits the canonical events directly. There
is no serialized intermediate union, no second pass. What survives is the
*observable contract* of `canonicalize_provider_stream`, reproduced exactly:

- the event order (`start`; then per content block a `*_start` / `*_delta` … and,
  at the end, `text_end` **before** `thinking_end`; then `done`);
- the content-index bookkeeping (`text_index` / `thinking_index` assigned lazily
  on first delta, tool calls appended in streamed order);
- the finish-reason mapping (`stream.py:44-49` → [`stream::map_finish_reason`]:
  tool-ish → `toolUse`, length-ish → `length`, else `stop`);
- the two terminal-error shapes — a provider error carrying a `provider_error`
  diagnostic with the HTTP `{status_code, body, attempts}` details, and the
  "Provider stream ended without a terminal event" error with usage reset — and
  the "emit `start` first even on an immediate error" rule that makes the
  OpenAI error golden read `["start", "error"]`.

Adapters hand the accumulator an in-process [`stream::Delta`] enum
(`Text`/`Thinking`/`ToolCall`/`End`/`Error`). That looks superficially like
`_provider_events`, and the distinction matters: `Delta` is never serialized,
never a pydantic model, never a wire type — it is a plain hand-off from a parser's
`feed_line` to the accumulator, gone the instant it is applied. The thing the plan
forbids (a *transitional serialized layer* with its own models and dump format) is
absent; the thing it requires (a shared accumulator with identical observable
semantics) is what `stream.rs` is.

### Snapshots: working-copy clone, not deep-copy

Every streaming event carries a `partial` snapshot of the assistant message built
so far. tau deep-copies the whole message per event (`model_copy(deep=True)`). rho
keeps one mutable working copy in the accumulator and clones it into each event.
Because M2's wire `AssistantMessageEvent` *owns* its `partial: AssistantMessage`
(by value, not behind an `Arc`), the clone is unavoidable at the type boundary —
but it is a clone of an already-built value, never a re-parse, and the mutation
happens once per delta on the working copy. Same observable protocol as tau's
per-event deep copy, without re-deriving the message each time.

### Who stamps the timestamp

tau makes fixtures reproducible by monkey-patching `messages.time` before
constructing any model, so every `AssistantMessage(...)` an adapter builds gets
the frozen `1_700_000_000_123` ms stamp from its `default_factory`. Rust has no
monkey-patch, and an adapter's SSE parser has no clock. So the **accumulator**
owns the clock: it stamps the `partial` at construction and stamps the final
`done`/`error` message in `response_end`/`error`. Providers are constructed with
an `Arc<dyn Clock>` (`with_clock` for tests → `FixedClock::fixture()`), exactly
mirroring how M2's harness threads the clock. This is the one place the port can't
be a literal transliteration — the `assistant_message(..., 0)` an adapter builds
carries a placeholder `0` that the accumulator overwrites — and it is called out
at both sites.

## SSE parsing by hand

No `eventsource` crate. tau hand-parses `data:` lines per adapter
(`anthropic.py:398-401` and friends), and the framing quirks differ enough that a
generic library would obscure them, so rho hand-parses too:

- **Line framing.** The engine's `LineSplitter` reproduces httpx `aiter_lines`
  over `reqwest`'s raw byte chunks: split on `\n`, strip a preceding `\r`, yield
  each line **without** its terminator, and yield a final unterminated line at
  EOF. It buffers across chunk boundaries at the byte level (so a multi-byte char
  split across two TCP reads is only decoded once the line is whole), which is why
  the goldens pass whether the mock server sends the body in one shot or byte by
  byte.
- **`data:` stripping differs by provider.** Most adapters `strip()` the line
  first, then `removeprefix("data:").strip()` (`util::parse_sse_line`); Anthropic
  does **not** left-strip (`util::parse_sse_line_no_lstrip`). Both are ported
  verbatim; a blank line is `None` either way.
- **Codex is multi-line.** ChatGPT-Codex frames each event as one or more `data:`
  lines terminated by a blank line (`openai_codex.py::_iter_sse_objects`). Its
  parser buffers `data:` payloads and flushes the joined object on the blank line
  (and once more at EOF for a trailing object without a blank terminator), whereas
  the OpenAI/Mistral chat parsers treat every `data:` line as a complete object
  and stop on `data: [DONE]`.
- **Invalid-JSON handling differs too.** Anthropic and the OpenAI chat parser
  surface an invalid chunk as a fatal `Provider returned invalid JSON chunk`
  error; Google and Mistral silently skip it (`_loads_object` → `None` → no
  event). Ported as-is.

Tool-call assembly is the fiddliest. Chat/Mistral index builders by the delta's
`index`; the OpenAI Responses parser orders builders by `output_index`; Codex
correlates streamed argument deltas to their call across three keys
(`item_id` → `call_id` → `output_index`, with a single-active-tool fallback) and
mints the final id as `call_id|item_id` — which is why the Codex tool golden id is
`call-1|fc1`. Each builder's empty-vs-`{}`-vs-`{"_raw_arguments": …}` argument
fallback matches tau exactly (`openai_compatible::parse_arguments`, shared by all
five HTTP adapters).

## The retry envelope, and a socket-free test seam

Every tau adapter wraps the same status/network retry ladder around a
per-endpoint parser. rho factors that once into `engine::run`, parameterized by a
fetcher, a parser factory, and an `is_retryable_status` predicate. Two subtleties
the port preserves:

- **Retries emit nothing.** `canonicalize` dropped `ProviderRetryEvent` at the Pi
  boundary (`stream.py:71-73`), so a retry produces *no* canonical output. The rho
  accumulator therefore never even hears about a retry — the only observable
  effect is that another HTTP attempt happens. `retry.rs` keeps the exact delay
  curve (`0.25 · 2^attempt`, capped) and cancellation-aware backoff, ported with
  unit tests off tau's `test_http.py`/`test_tau_ai.py`; the end-to-end ladder
  (500→200 retry, 400 no-retry, cancel-aborts-backoff) is exercised in
  `tests/retry.rs`.
- **The fetcher is the seam.** `engine::run` takes a `ClosureFetcher` returning an
  `HttpResponse { status, body: stream-of-bytes }`. Production wires it to
  `send_reqwest`; the golden tests wire it — indirectly — to the in-process
  `mock-provider` server. That means the whole envelope (retry, line splitting,
  cancellation, the post-loop `acc.finish()` that mirrors `canonicalize`'s
  end-of-stream block) is exercised over **real HTTP with no live network**: the
  15 SSE goldens run the real `reqwest` path against a localhost server replaying
  the recorded body, and the retry tests use a per-attempt-scripted variant.

`tools/mock-provider` is that server (an `axum` bin + lib): it answers any `POST`
path with a configured status + body, chunkable with a size and per-chunk latency
(for M6 benchmarks), and captures request bodies. It has **no** dependency on
`rho-ai`/`rho-agent` — pure bytes in, bytes out — so `rho-ai`'s dev-deps can use
it without a cycle.

## Request-payload key-order discipline

The request goldens were captured through tau's `compact(payload)` —
`json.dumps(…, ensure_ascii=False, separators=(",", ":"))` over a dict — so the
byte layout *is* Python dict insertion order rendered compactly. `serde_json`
with the workspace's `preserve_order` feature is exactly that: an `IndexMap`
object rendered by `to_string` (compact, non-ASCII kept verbatim). So each request
builder inserts keys in tau's precise order — e.g. chat completions is
`model, stream, messages, stream_options, store, [max_tokens], [provider],
[reasoning*], [tools]`; the Responses body leads with `model, stream, store,
instructions, input, …`; Codex with `model, store, stream, instructions, input,
text, include, tool_choice, parallel_tool_calls, …`. A single transposed key is a
byte diff, so the builders are deliberately written as ordered `Map` inserts, not
`derive`d structs.

Some payload paths are ported faithfully even though no golden pins them (the
extraction never set `reasoning_effort`/`max_tokens`): the OpenAI reasoning
formats, Anthropic's thinking budget/adaptive modes, Mistral's reasoning routing,
and Google's `thinkingConfig` — the full `_google_thinking_config` model-name
lookup (gemini-2.5 budgets, gemini-3/gemma-4 levels, the `none`/`xhigh` special
cases). These are unit-tested against tau's tables rather than a byte fixture,
but the payload key order still matches tau so a future golden would pass.

One trap the port carries but no golden yet exercises: tau serializes a tool
call's `arguments` into a **string** via `json.dumps(arguments)` — Python's
*default* separators (`", "` / `": "`) and `ensure_ascii=True`. That string is
value content inside the request body, so its spaces and `\uXXXX` escapes survive
the outer compact re-serialization. `wire::python_dumps` reproduces Python's
default `json.dumps` (including astral-plane surrogate-pair escaping) for exactly
this, so multi-turn assistant-tool-call replays (M4+) stay byte-faithful.

## Codex OAuth design, and the manual live checklist

Anthropic, OpenAI-compatible, and Codex all support a per-request credential
resolver (`RuntimeProviderAuthResolver` / `OpenAICodexCredentialResolver`) —
tau's `Callable[[], Awaitable[...]]`, modeled as
`Arc<dyn Fn() -> BoxFuture<Result<_, String>>>`: async (a token can be refreshed
per call), shareable (the provider is `Send + Sync`), and **fallible**. Fallibility
is the one deliberate shape change: tau lets a resolver exception propagate, and
Codex wraps its whole attempt in `except Exception` to surface it as an error
event. rho makes the resolver return `Result<_, String>`; a resolver failure
becomes a non-retryable `FetchError`, which the engine turns into a terminal
`AssistantErrorEvent` — matching Codex's observable behavior and keeping errors as
data (the M2 philosophy) everywhere.

The resolver runs **inside** each attempt (so a mid-run token refresh is honored),
which is why the fetch closure — not the sync `stream_response` body — resolves
credentials, base-URL overrides, and headers. The token-refresh machinery is unit
tested against the recorded Codex fixtures (`_codex_creds` returns a fixed
`access-token`/`account-1`, exactly as the extraction does).

The *interactive* OAuth device flow (opening a browser, exchanging a code, caching
and refreshing the token) is out of scope for a byte-compat port and can't be
golden-tested. It is left to the application layer (M4). The manual checklist to
validate the live path when that lands:

1. Obtain a ChatGPT-subscription access token + `account_id` out of band (the
   `codex` CLI's cached credentials, or the device-code flow).
2. Build `OpenAICodexConfig::new(resolver)` where `resolver` returns those live
   credentials, leaving `base_url` at its default (`https://chatgpt.com/backend-api`).
3. Send a one-line prompt with no tools; confirm `text`/`reasoning` deltas stream
   and a `done` arrives (the URL resolves to `…/codex/responses`, headers carry
   `Authorization: Bearer …`, `chatgpt-account-id`, `originator`, `OpenAI-Beta:
   responses=experimental`).
4. Send a prompt that forces a tool call; confirm the `toolCall` id is
   `call_id|item_id` and arguments parse.
5. Force a 401 (expired token) and confirm the resolver is re-invoked on the next
   turn; force a terminal 429 (a `GoUsageLimitError`-class body) and confirm it is
   **not** retried (`openai_codex::is_terminal_rate_limit`).

## reqwest / rustls / socks

The workspace pins `reqwest` with `rustls-tls` (no OpenSSL), `stream`, `socks`,
and `json`. tau normalizes proxy **environment variables** before constructing an
httpx client, rewriting the generic `socks://` scheme (which httpx rejects) to
`socks5://`. reqwest takes proxies through its builder instead, so `http::create_client`
reads the same env vars, applies the ported `normalize_proxy_url`, and installs
explicit `reqwest::Proxy` entries (with `.no_proxy()` first to take full control).
The pure `normalize_proxy_url` is a direct port of tau's helper and its tests; the
SOCKS support itself is the crate feature.

## The `FakeProvider` placement

tau's `FakeProvider` lives in `tau_ai`; rho keeps it in `rho-agent` (behind the
default-on `fake` feature) because the M2 loop/harness goldens must drive the
provider seam and `rho-agent` must not depend on `rho-ai`. For API parity M3
re-exports it as `rho_ai::FakeProvider`, so `rho_ai::FakeProvider` matches tau's
`tau_ai.FakeProvider` import path while the type still lives one layer down. Its
golden (`fixtures/sse/fake/`) round-trips the recorded event script through the
re-exported provider unchanged.
