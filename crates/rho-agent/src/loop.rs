//! The pure Pi-compatible provider/tool agent loop (tau `tau_agent/loop.py`).
//!
//! ## Async generator → `async-stream`
//!
//! tau's `run_agent_loop` is an `async def ... yield` generator: it `await`s
//! provider streams and tool futures and `yield`s [`AgentEvent`]s in between. rho
//! renders it with the [`async_stream::stream!`] macro, whose `yield` + `.await`
//! desugars to the same cooperative state machine. The whole loop is therefore
//! one `stream!` block; tau's helper sub-generators (`_assistant_events`,
//! `_execute_tool_call`) are **inlined**, because `stream!` has no `yield from`
//! and the event ordering is easier to keep exact in one body. `_run_tool` stays
//! a plain `async fn` (it buffers updates, it does not yield).
//!
//! ## The shared message list
//!
//! tau threads the *same* `list[AgentMessage]` object through the loop, the
//! provider (which snapshots it per call), and the caller (which reads the
//! appended messages afterward). rho renders that shared-mutable list as
//! `Arc<Mutex<Vec<AgentMessage>>>`: the loop appends to it live, the provider
//! gets a `clone()` snapshot each call, and the harness/caller observe the
//! appends. The lock is never held across an `.await` (snapshot-then-release).
//!
//! ## Errors are data
//!
//! A provider failure arrives as an [`AssistantMessageEvent::Error`] and becomes
//! a `stop_reason = "error"` assistant message that ends the run; a tool failure
//! becomes an `is_error` tool result. Neither ever propagates out of the stream —
//! the loop's item type is `AgentEvent`, not `Result<AgentEvent, _>` (matching
//! `stream.py` / `loop.py:284-285`).
//!
//! ## Tools run sequentially
//!
//! Despite `AgentTool.execution_mode`, `tau_agent.loop` executes a turn's tool
//! calls one after another (no `gather`/`TaskGroup`). rho matches: a plain `for`
//! over the calls. See `dev-notes/phase-2.md`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_stream::stream;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::Stream;

use crate::clock::Clock;
use crate::events::{
    AgentEndEvent, AgentEvent, AgentStartEvent, MessageEndEvent, MessageStartEvent,
    MessageUpdateEvent, ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
    TurnEndEvent, TurnStartEvent,
};
use crate::messages::{AgentMessage, AssistantMessage, StopReason, ToolCall, ToolResultMessage};
use crate::provider::{CancellationToken, ModelProvider};
use crate::provider_events::AssistantMessageEvent;
use crate::tools::{AgentTool, AgentToolResult, error_result};

/// Drains queued messages for one turn (tau `get_steering_messages` /
/// `get_follow_up_messages`). Called synchronously between turns.
pub type QueueDrain = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

/// A pre-tool-call hook (tau `BeforeToolCall`): returns `(blocked, reason)`.
pub type BeforeToolCall =
    Arc<dyn Fn(ToolCall) -> BoxFuture<'static, (bool, Option<String>)> + Send + Sync>;

/// A post-tool-call hook (tau `AfterToolCall`): may rewrite `(result, is_error)`.
pub type AfterToolCall = Arc<
    dyn Fn(ToolCall, AgentToolResult, bool) -> BoxFuture<'static, (AgentToolResult, bool)>
        + Send
        + Sync,
>;

/// Parameters for [`run_agent_loop`], grouped to keep the call site readable
/// (Rust has no keyword arguments; this mirrors tau's keyword-only signature).
pub struct AgentLoopConfig {
    /// The model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// Requested model id.
    pub model: String,
    /// System prompt.
    pub system: String,
    /// The shared, live-mutated transcript (tau's in/out `messages` list).
    pub messages: Arc<Mutex<Vec<AgentMessage>>>,
    /// Tools available this run.
    pub tools: Vec<AgentTool>,
    /// Prompts to inject at the start of the run.
    pub prompts: Vec<AgentMessage>,
    /// Optional turn cap; exceeding it ends the run with an error message.
    pub max_turns: Option<i64>,
    /// Optional polled cancellation signal.
    pub signal: Option<Arc<dyn CancellationToken>>,
    /// Optional steering-message drain (per turn).
    pub get_steering_messages: Option<QueueDrain>,
    /// Optional follow-up-message drain (when the loop would otherwise stop).
    pub get_follow_up_messages: Option<QueueDrain>,
    /// Optional pre-tool-call hook.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional post-tool-call hook.
    pub after_tool_call: Option<AfterToolCall>,
    /// Clock for message timestamps on loop-authored messages.
    pub clock: Arc<dyn Clock>,
}

/// Run the provider/tool loop and emit Pi-compatible agent events.
///
/// A transliteration of tau's `run_agent_loop`; the returned stream yields the
/// **exact** [`AgentEvent`] sequence tau emits (verified byte-for-byte against
/// `fixtures/event-streams/`). See the module docs for the shared-list and
/// error-as-data contracts.
// One long body on purpose: this is a 1:1 transliteration of tau's single
// `run_agent_loop` generator, and splitting it would obscure the event ordering
// that is the whole contract.
#[allow(clippy::too_many_lines)]
pub fn run_agent_loop(config: AgentLoopConfig) -> impl Stream<Item = AgentEvent> {
    let AgentLoopConfig {
        provider,
        model,
        system,
        messages,
        tools,
        prompts,
        max_turns,
        signal,
        get_steering_messages,
        get_follow_up_messages,
        before_tool_call,
        after_tool_call,
        clock,
    } = config;

    stream! {
        // new_messages = list(prompts); messages.extend(prompts)
        let mut new_messages: Vec<AgentMessage> = prompts.clone();
        if !prompts.is_empty() {
            messages.lock().expect("messages lock").extend(prompts.clone());
        }

        yield AgentEvent::AgentStart(AgentStartEvent::new());
        yield AgentEvent::TurnStart(TurnStartEvent::new());
        for prompt in &prompts {
            yield AgentEvent::MessageStart(MessageStartEvent::new(prompt.clone()));
            yield AgentEvent::MessageEnd(MessageEndEvent::new(prompt.clone()));
        }

        if let Some(mt) = max_turns {
            if mt < 1 {
                let error = error_message(&model, "max_turns must be at least 1", clock.as_ref());
                push_both(&messages, &mut new_messages, AgentMessage::Assistant(error.clone()));
                yield AgentEvent::MessageStart(MessageStartEvent::new(AgentMessage::Assistant(error.clone())));
                yield AgentEvent::MessageEnd(MessageEndEvent::new(AgentMessage::Assistant(error.clone())));
                yield AgentEvent::TurnEnd(TurnEndEvent::new(AgentMessage::Assistant(error), Vec::new()));
                yield AgentEvent::AgentEnd(AgentEndEvent::new(new_messages));
                return;
            }
        }

        let tool_by_name: HashMap<String, AgentTool> =
            tools.iter().map(|t| (t.name.clone(), t.clone())).collect();
        let mut turn: i64 = 1;
        let mut first_turn = true;
        let mut pending: Vec<AgentMessage> = drain(get_steering_messages.as_ref());

        'outer: loop {
            let mut has_more_tools = true;
            while has_more_tools || !pending.is_empty() {
                if !first_turn {
                    yield AgentEvent::TurnStart(TurnStartEvent::new());
                }
                first_turn = false;

                for message in std::mem::take(&mut pending) {
                    push_both(&messages, &mut new_messages, message.clone());
                    yield AgentEvent::MessageStart(MessageStartEvent::new(message.clone()));
                    yield AgentEvent::MessageEnd(MessageEndEvent::new(message));
                }

                if let Some(mt) = max_turns {
                    if turn > mt {
                        let msg = format!("Agent stopped after max_turns={mt}");
                        let error = error_message(&model, &msg, clock.as_ref());
                        push_both(&messages, &mut new_messages, AgentMessage::Assistant(error.clone()));
                        yield AgentEvent::MessageStart(MessageStartEvent::new(AgentMessage::Assistant(error.clone())));
                        yield AgentEvent::MessageEnd(MessageEndEvent::new(AgentMessage::Assistant(error.clone())));
                        yield AgentEvent::TurnEnd(TurnEndEvent::new(AgentMessage::Assistant(error), Vec::new()));
                        yield AgentEvent::AgentEnd(AgentEndEvent::new(new_messages));
                        return;
                    }
                }

                // --- assistant events (inlined `_assistant_events`) ---------
                let snapshot = messages.lock().expect("messages lock").clone();
                let mut source =
                    provider.stream_response(&model, &system, &snapshot, &tools, signal.clone());
                let mut assistant: Option<AssistantMessage> = None;
                let mut started = false;
                while let Some(event) = source.next().await {
                    match event {
                        AssistantMessageEvent::Start(e) => {
                            started = true;
                            yield AgentEvent::MessageStart(MessageStartEvent::new(
                                AgentMessage::Assistant(e.partial),
                            ));
                        }
                        AssistantMessageEvent::Done(e) => {
                            if !started {
                                yield AgentEvent::MessageStart(MessageStartEvent::new(
                                    AgentMessage::Assistant(e.message.clone()),
                                ));
                            }
                            yield AgentEvent::MessageEnd(MessageEndEvent::new(
                                AgentMessage::Assistant(e.message.clone()),
                            ));
                            assistant = Some(e.message);
                        }
                        AssistantMessageEvent::Error(e) => {
                            if !started {
                                yield AgentEvent::MessageStart(MessageStartEvent::new(
                                    AgentMessage::Assistant(e.error.clone()),
                                ));
                            }
                            yield AgentEvent::MessageEnd(MessageEndEvent::new(
                                AgentMessage::Assistant(e.error.clone()),
                            ));
                            assistant = Some(e.error);
                        }
                        other => {
                            let partial = other.partial().clone();
                            yield AgentEvent::MessageUpdate(MessageUpdateEvent::new(
                                AgentMessage::Assistant(partial),
                                other,
                            ));
                        }
                    }
                }

                let assistant = if let Some(a) = assistant {
                    a
                } else {
                    // Defensive: a well-behaved provider always terminates.
                    let a = error_message(
                        &model,
                        "Provider produced no assistant message",
                        clock.as_ref(),
                    );
                    yield AgentEvent::MessageStart(MessageStartEvent::new(
                        AgentMessage::Assistant(a.clone()),
                    ));
                    yield AgentEvent::MessageEnd(MessageEndEvent::new(
                        AgentMessage::Assistant(a.clone()),
                    ));
                    a
                };

                push_both(&messages, &mut new_messages, AgentMessage::Assistant(assistant.clone()));

                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
                    yield AgentEvent::TurnEnd(TurnEndEvent::new(
                        AgentMessage::Assistant(assistant),
                        Vec::new(),
                    ));
                    yield AgentEvent::AgentEnd(AgentEndEvent::new(new_messages));
                    return;
                }

                // --- tool calls (inlined `_execute_tool_call`), sequential ---
                let mut tool_results: Vec<ToolResultMessage> = Vec::new();
                let calls = assistant.tool_calls();
                has_more_tools = !calls.is_empty();
                for call in calls {
                    yield AgentEvent::ToolExecutionStart(ToolExecutionStartEvent::new(
                        call.id.clone(),
                        call.name.clone(),
                        call.arguments.clone(),
                    ));

                    let mut result;
                    let is_error;

                    let (blocked, block_reason) = match &before_tool_call {
                        Some(hook) => hook(call.clone()).await,
                        None => (false, None),
                    };

                    if blocked {
                        result = error_result(
                            block_reason.unwrap_or_else(|| "Tool execution was blocked".to_string()),
                        );
                        is_error = true;
                    } else if signal.as_ref().is_some_and(|s| s.is_cancelled()) {
                        result = error_result("Operation aborted");
                        is_error = true;
                    } else if let Some(tool) = tool_by_name.get(&call.name) {
                        let (r, e, updates) = run_tool(tool, &call, signal.clone()).await;
                        for update in updates {
                            yield AgentEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent::new(
                                call.id.clone(),
                                call.name.clone(),
                                call.arguments.clone(),
                                update,
                            ));
                        }
                        result = r;
                        is_error = e;
                    } else {
                        result = error_result(format!("Tool {} not found", call.name));
                        is_error = true;
                    }

                    let is_error = if let Some(hook) = &after_tool_call {
                        let (r, e) = hook(call.clone(), result, is_error).await;
                        result = r;
                        e
                    } else {
                        is_error
                    };

                    yield AgentEvent::ToolExecutionEnd(ToolExecutionEndEvent::new(
                        call.id.clone(),
                        call.name.clone(),
                        result.clone(),
                        is_error,
                    ));

                    let mut message =
                        ToolResultMessage::new(call.id.clone(), call.name.clone(), result.content);
                    message.details = result.details;
                    message.added_tool_names = result.added_tool_names;
                    message.is_error = is_error;
                    message.timestamp = clock.now_ms();

                    yield AgentEvent::MessageStart(MessageStartEvent::new(
                        AgentMessage::ToolResult(message.clone()),
                    ));
                    yield AgentEvent::MessageEnd(MessageEndEvent::new(
                        AgentMessage::ToolResult(message.clone()),
                    ));
                    tool_results.push(message.clone());
                    push_both(&messages, &mut new_messages, AgentMessage::ToolResult(message));
                }

                yield AgentEvent::TurnEnd(TurnEndEvent::new(
                    AgentMessage::Assistant(assistant),
                    tool_results,
                ));
                turn += 1;
                pending = drain(get_steering_messages.as_ref());
            }

            let follow_ups = drain(get_follow_up_messages.as_ref());
            if !follow_ups.is_empty() {
                pending = follow_ups;
                continue 'outer;
            }
            break;
        }

        yield AgentEvent::AgentEnd(AgentEndEvent::new(new_messages));
    }
}

/// Run one tool, buffering its progress updates (tau `_run_tool`).
///
/// The synchronous `on_update` callback accumulates deep copies while
/// `accepting`; the flag is flipped off once the tool returns, so late updates
/// are dropped (matching tau's `finally: accepting = False`). A tool failure
/// (`Err`) becomes an `is_error` result — see the module's "errors are data".
async fn run_tool(
    tool: &AgentTool,
    call: &ToolCall,
    signal: Option<Arc<dyn CancellationToken>>,
) -> (AgentToolResult, bool, Vec<AgentToolResult>) {
    let updates: Arc<Mutex<Vec<AgentToolResult>>> = Arc::new(Mutex::new(Vec::new()));
    let accepting = Arc::new(AtomicBool::new(true));

    let on_update = {
        let updates = updates.clone();
        let accepting = accepting.clone();
        Arc::new(move |partial: AgentToolResult| {
            if accepting.load(Ordering::SeqCst) {
                updates.lock().expect("updates lock").push(partial);
            }
        })
    };

    let outcome = tool
        .execute(call.id.clone(), call.arguments.clone(), signal, on_update)
        .await;
    accepting.store(false, Ordering::SeqCst);

    let updates = Arc::try_unwrap(updates).map_or_else(
        |arc| arc.lock().expect("updates lock").clone(),
        |m| m.into_inner().expect("updates lock"),
    );

    match outcome {
        Ok(result) => (result, false, updates),
        Err(err) => (error_result(err.to_string()), true, updates),
    }
}

/// Build an errored assistant message (tau `_error_message`): empty content,
/// `stop_reason = "error"`, the given `error_message`, and a clock timestamp.
fn error_message(model: &str, message: &str, clock: &dyn Clock) -> AssistantMessage {
    let mut m = AssistantMessage::new(Vec::new())
        .with_model(model)
        .with_stop_reason(StopReason::Error)
        .with_error_message(message);
    m.timestamp = clock.now_ms();
    m
}

/// Append `message` to both the shared transcript and the run's `new_messages`.
fn push_both(
    messages: &Arc<Mutex<Vec<AgentMessage>>>,
    new_messages: &mut Vec<AgentMessage>,
    message: AgentMessage,
) {
    messages
        .lock()
        .expect("messages lock")
        .push(message.clone());
    new_messages.push(message);
}

/// Call an optional queue drain, returning its messages (or empty).
fn drain(drain_fn: Option<&QueueDrain>) -> Vec<AgentMessage> {
    drain_fn.map_or_else(Vec::new, |f| f())
}
