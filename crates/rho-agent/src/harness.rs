//! Stateful reusable agent harness built on the loop (tau
//! `tau_agent/harness.py`).
//!
//! The harness wraps [`run_agent_loop`] with transcript state, a subscriber
//! list, and steering/follow-up queues. Its shape follows tau closely, rendered
//! in Rust idioms:
//!
//! * The transcript is the `Arc<Mutex<Vec<AgentMessage>>>` the loop mutates live,
//!   so `messages()` reflects a run's appends without reconstruction.
//! * `subscribe` returns a boxed unsubscribe closure (tau returns a callable);
//!   listeners are keyed by a monotonic id so the closure removes exactly one.
//! * `prompt`/`prompt_message`/`continue_` return `Result<_, HarnessError>` where
//!   tau raises `RuntimeError` — the overlap guard is a synchronous check before
//!   the stream is created, exactly as in tau.
//! * Listeners are synchronous (`Fn(&AgentEvent)`). tau also awaits async
//!   listeners; no ported test or fixture uses one, so that is deferred (noted in
//!   `dev-notes/phase-2.md`).

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_stream::stream;
use futures::StreamExt;
use futures::stream::Stream;

use crate::agent_loop::{
    AfterToolCall, AgentLoopConfig, BeforeToolCall, QueueDrain, run_agent_loop,
};
use crate::clock::{Clock, system_clock};
use crate::events::AgentEvent;
use crate::messages::{
    AgentMessage, TextContent, ToolResultContent, ToolResultMessage, UserMessage,
};
use crate::provider::{CancellationToken, ModelProvider, SimpleCancellationToken};
use crate::tools::AgentTool;

/// A synchronous event listener (tau `EventListener`, sync arm).
pub type EventListener = Arc<dyn Fn(&AgentEvent) + Send + Sync>;

/// The boxed unsubscribe closure `subscribe` returns.
pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

/// The event stream a run yields.
pub type EventStream = std::pin::Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// How queued messages are drained per turn (tau `QueueMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueMode {
    /// Pop a single message (the oldest) per turn (`popleft`).
    #[default]
    OneAtATime,
    /// Drain the whole queue at once.
    All,
}

/// A snapshot of both queues (tau `QueuedMessages`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueuedMessages {
    /// Queued steering messages (oldest first).
    pub steering: Vec<AgentMessage>,
    /// Queued follow-up messages (oldest first).
    pub follow_up: Vec<AgentMessage>,
}

impl QueuedMessages {
    /// Total queued messages across both queues.
    #[must_use]
    pub fn count(&self) -> usize {
        self.steering.len() + self.follow_up.len()
    }
}

/// The harness raised its already-running guard (tau `RuntimeError`).
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// A run is already in progress.
    #[error("AgentHarness is already running; use steer() or follow_up() to queue messages.")]
    AlreadyRunning,
}

/// Configuration for an [`AgentHarness`] (tau `AgentHarnessConfig`).
#[derive(Clone)]
pub struct AgentHarnessConfig {
    /// The model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// Requested model id.
    pub model: String,
    /// System prompt.
    pub system: String,
    /// Available tools.
    pub tools: Vec<AgentTool>,
    /// Optional turn cap.
    pub max_turns: Option<i64>,
    /// How queued messages are drained.
    pub queue_mode: QueueMode,
    /// Optional pre-tool-call hook.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional post-tool-call hook.
    pub after_tool_call: Option<AfterToolCall>,
    /// Clock for harness/loop-authored message timestamps. Defaults to real time
    /// ([`system_clock`]); tests pin it for reproducible goldens.
    pub clock: Arc<dyn Clock>,
}

impl AgentHarnessConfig {
    /// Build a config with tau's defaults (no tools, no cap, one-at-a-time
    /// queueing, no hooks, real-time clock).
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
        system: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            system: system.into(),
            tools: Vec::new(),
            max_turns: None,
            queue_mode: QueueMode::OneAtATime,
            before_tool_call: None,
            after_tool_call: None,
            clock: system_clock(),
        }
    }

    /// Set the tools.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<AgentTool>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the turn cap.
    #[must_use]
    pub fn with_max_turns(mut self, max_turns: Option<i64>) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Set the queue mode.
    #[must_use]
    pub fn with_queue_mode(mut self, queue_mode: QueueMode) -> Self {
        self.queue_mode = queue_mode;
        self
    }

    /// Pin the clock (tests / fixture reproduction).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// A reusable stateful agent brain independent of coding/UI policy (tau
/// `AgentHarness`).
pub struct AgentHarness {
    config: AgentHarnessConfig,
    messages: Arc<Mutex<Vec<AgentMessage>>>,
    listeners: Arc<Mutex<Vec<(u64, EventListener)>>>,
    listener_counter: AtomicU64,
    current_signal: Arc<Mutex<Option<Arc<SimpleCancellationToken>>>>,
    running: Arc<AtomicBool>,
    steering_queue: Arc<Mutex<VecDeque<AgentMessage>>>,
    follow_up_queue: Arc<Mutex<VecDeque<AgentMessage>>>,
}

impl AgentHarness {
    /// Build a harness with an optional starting transcript.
    pub fn new(config: AgentHarnessConfig, messages: Vec<AgentMessage>) -> Self {
        Self {
            config,
            messages: Arc::new(Mutex::new(messages)),
            listeners: Arc::new(Mutex::new(Vec::new())),
            listener_counter: AtomicU64::new(0),
            current_signal: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            follow_up_queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// The current transcript (a snapshot).
    #[must_use]
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.messages.lock().expect("messages lock").clone()
    }

    /// The harness configuration.
    #[must_use]
    pub fn config(&self) -> &AgentHarnessConfig {
        &self.config
    }

    /// Whether a run is currently in progress.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// A snapshot of both queues.
    #[must_use]
    pub fn queued_messages(&self) -> QueuedMessages {
        QueuedMessages {
            steering: self
                .steering_queue
                .lock()
                .expect("steering lock")
                .iter()
                .cloned()
                .collect(),
            follow_up: self
                .follow_up_queue
                .lock()
                .expect("follow_up lock")
                .iter()
                .cloned()
                .collect(),
        }
    }

    /// Total queued messages.
    #[must_use]
    pub fn pending_message_count(&self) -> usize {
        self.queued_messages().count()
    }

    /// Whether either queue is non-empty.
    #[must_use]
    pub fn has_queued_messages(&self) -> bool {
        !self
            .steering_queue
            .lock()
            .expect("steering lock")
            .is_empty()
            || !self
                .follow_up_queue
                .lock()
                .expect("follow_up lock")
                .is_empty()
    }

    /// Append a message to the transcript.
    pub fn append_message(&self, message: AgentMessage) {
        self.messages.lock().expect("messages lock").push(message);
    }

    /// Replace the whole transcript.
    pub fn replace_messages(&self, messages: Vec<AgentMessage>) {
        *self.messages.lock().expect("messages lock") = messages;
    }

    /// Subscribe a listener; returns a closure that removes it.
    pub fn subscribe(&self, listener: EventListener) -> Unsubscribe {
        let id = self.listener_counter.fetch_add(1, Ordering::SeqCst);
        self.listeners
            .lock()
            .expect("listeners lock")
            .push((id, listener));
        let listeners = self.listeners.clone();
        Box::new(move || {
            listeners
                .lock()
                .expect("listeners lock")
                .retain(|(existing, _)| *existing != id);
        })
    }

    /// Cancel the in-flight run (if any).
    pub fn cancel(&self) {
        if let Some(signal) = self.current_signal.lock().expect("signal lock").as_ref() {
            signal.cancel();
        }
    }

    /// Queue a steering message from a string (tau `steer`).
    pub fn steer(&self, content: &str) -> QueuedMessages {
        self.steer_message(AgentMessage::User(self.user_message(content)))
    }

    /// Queue a steering message (tau `steer_message`).
    pub fn steer_message(&self, message: AgentMessage) -> QueuedMessages {
        self.steering_queue
            .lock()
            .expect("steering lock")
            .push_back(message);
        self.queued_messages()
    }

    /// Queue a follow-up message from a string (tau `follow_up`).
    pub fn follow_up(&self, content: &str) -> QueuedMessages {
        self.follow_up_message(AgentMessage::User(self.user_message(content)))
    }

    /// Queue a follow-up message (tau `follow_up_message`).
    pub fn follow_up_message(&self, message: AgentMessage) -> QueuedMessages {
        self.follow_up_queue
            .lock()
            .expect("follow_up lock")
            .push_back(message);
        self.queued_messages()
    }

    /// Clear both queues, returning their prior contents (tau `clear_queues`).
    pub fn clear_queues(&self) -> QueuedMessages {
        let snapshot = self.queued_messages();
        self.steering_queue.lock().expect("steering lock").clear();
        self.follow_up_queue.lock().expect("follow_up lock").clear();
        snapshot
    }

    /// Pop the most recently queued follow-up (tau `pop_latest_follow_up`).
    pub fn pop_latest_follow_up(&self) -> Option<AgentMessage> {
        self.follow_up_queue
            .lock()
            .expect("follow_up lock")
            .pop_back()
    }

    /// Pop the most recently queued steering message (tau `pop_latest_steering`).
    pub fn pop_latest_steering(&self) -> Option<AgentMessage> {
        self.steering_queue
            .lock()
            .expect("steering lock")
            .pop_back()
    }

    /// Start a run seeded with one prompt message (tau `prompt_message`).
    pub fn prompt_message(&self, message: AgentMessage) -> Result<EventStream, HarnessError> {
        self.ensure_not_running()?;
        append_interrupted_tool_results(&self.messages, self.config.clock.as_ref());
        self.running.store(true, Ordering::SeqCst);
        Ok(self.run(vec![message]))
    }

    /// Start a run seeded with one user prompt string (tau `prompt`).
    pub fn prompt(&self, content: &str) -> Result<EventStream, HarnessError> {
        self.prompt_message(AgentMessage::User(self.user_message(content)))
    }

    /// Continue the run with no new prompt (tau `continue_`).
    pub fn continue_(&self) -> Result<EventStream, HarnessError> {
        self.ensure_not_running()?;
        append_interrupted_tool_results(&self.messages, self.config.clock.as_ref());
        self.running.store(true, Ordering::SeqCst);
        Ok(self.run(Vec::new()))
    }

    /// Append synthetic error tool-results for any interrupted tool calls,
    /// returning how many were added (tau `append_interrupted_tool_results`).
    pub fn append_interrupted_tool_results(&self) -> usize {
        append_interrupted_tool_results(&self.messages, self.config.clock.as_ref())
    }

    // --- internals ----------------------------------------------------------

    fn user_message(&self, content: &str) -> UserMessage {
        let mut m = UserMessage::new(content);
        m.timestamp = self.config.clock.now_ms();
        m
    }

    fn ensure_not_running(&self) -> Result<(), HarnessError> {
        if self.running.load(Ordering::SeqCst) {
            Err(HarnessError::AlreadyRunning)
        } else {
            Ok(())
        }
    }

    fn run(&self, prompts: Vec<AgentMessage>) -> EventStream {
        let signal = Arc::new(SimpleCancellationToken::new());
        *self.current_signal.lock().expect("signal lock") = Some(signal.clone());

        let messages = self.messages.clone();
        let listeners = self.listeners.clone();
        let running = self.running.clone();
        let current_signal = self.current_signal.clone();
        let clock = self.config.clock.clone();

        let steering: QueueDrain = {
            let queue = self.steering_queue.clone();
            let mode = self.config.queue_mode;
            Arc::new(move || drain_queue(&queue, mode))
        };
        let follow_up: QueueDrain = {
            let queue = self.follow_up_queue.clone();
            let mode = self.config.queue_mode;
            Arc::new(move || drain_queue(&queue, mode))
        };

        let inner = run_agent_loop(AgentLoopConfig {
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            system: self.config.system.clone(),
            messages: messages.clone(),
            tools: self.config.tools.clone(),
            prompts,
            max_turns: self.config.max_turns,
            signal: Some(signal.clone() as Arc<dyn CancellationToken>),
            get_steering_messages: Some(steering),
            get_follow_up_messages: Some(follow_up),
            before_tool_call: self.config.before_tool_call.clone(),
            after_tool_call: self.config.after_tool_call.clone(),
            clock: clock.clone(),
        });

        Box::pin(stream! {
            futures::pin_mut!(inner);
            while let Some(event) = inner.next().await {
                notify(&listeners, &event);
                yield event;
            }
            // finally: repair on cancellation, clear the signal, unset running.
            if signal.is_cancelled() {
                append_interrupted_tool_results(&messages, clock.as_ref());
            }
            {
                let mut current = current_signal.lock().expect("signal lock");
                if current
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, &signal))
                {
                    *current = None;
                }
            }
            running.store(false, Ordering::SeqCst);
        })
    }
}

/// Notify listeners with a snapshot (so a listener may (un)subscribe safely).
fn notify(listeners: &Arc<Mutex<Vec<(u64, EventListener)>>>, event: &AgentEvent) {
    let snapshot: Vec<EventListener> = listeners
        .lock()
        .expect("listeners lock")
        .iter()
        .map(|(_, listener)| listener.clone())
        .collect();
    for listener in snapshot {
        listener(event);
    }
}

/// Drain one turn's worth of messages from `queue` per `mode` (tau
/// `_drain_queue`): all at once, or a single `popleft`.
fn drain_queue(queue: &Arc<Mutex<VecDeque<AgentMessage>>>, mode: QueueMode) -> Vec<AgentMessage> {
    let mut guard = queue.lock().expect("queue lock");
    if guard.is_empty() {
        return Vec::new();
    }
    match mode {
        QueueMode::All => guard.drain(..).collect(),
        QueueMode::OneAtATime => guard.pop_front().into_iter().collect(),
    }
}

/// Append synthetic `is_error` tool-results for every assistant tool call that
/// never received one (tau `_append_interrupted_tool_results`). Returns the count
/// added.
fn append_interrupted_tool_results(
    messages: &Arc<Mutex<Vec<AgentMessage>>>,
    clock: &dyn Clock,
) -> usize {
    let mut guard = messages.lock().expect("messages lock");

    let mut returned_ids: HashSet<String> = guard
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult(t) => Some(t.tool_call_id.clone()),
            _ => None,
        })
        .collect();

    let mut to_add: Vec<AgentMessage> = Vec::new();
    for message in guard.iter() {
        let AgentMessage::Assistant(assistant) = message else {
            continue;
        };
        for call in assistant.tool_calls() {
            if returned_ids.contains(&call.id) {
                continue;
            }
            returned_ids.insert(call.id.clone());
            let mut repair = ToolResultMessage::new(
                call.id.clone(),
                call.name.clone(),
                vec![ToolResultContent::Text(TextContent::new(
                    "Tool call interrupted by user",
                ))],
            );
            repair.is_error = true;
            repair.timestamp = clock.now_ms();
            to_add.push(AgentMessage::ToolResult(repair));
        }
    }

    let added = to_add.len();
    guard.extend(to_add);
    added
}
