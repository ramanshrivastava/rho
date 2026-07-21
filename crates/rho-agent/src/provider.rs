//! Provider contract owned by the portable agent layer (tau
//! `tau_agent/provider.py`).
//!
//! ## `stream_response` is sync → `BoxStream`
//!
//! tau's `ModelProvider.stream_response` is a **synchronous** function that
//! *returns* an `AsyncIterator[AssistantMessageEvent]` — it does the provider
//! bookkeeping (record the call, snapshot the messages) synchronously and hands
//! back a lazy async stream. rho keeps that exact shape: a sync method returning
//! a [`AssistantEventStream`] (`BoxStream<'static, AssistantMessageEvent>`). The
//! returned stream is `'static` because it outlives the borrowed request slices —
//! a provider that needs the messages/tools must snapshot (clone) them
//! synchronously in the method body, precisely as tau's `FakeProvider` does
//! (`list(messages)`).
//!
//! ## Cancellation is *polled*, never awaited
//!
//! tau's `CancellationToken` is a `Protocol` with a single synchronous
//! `is_cancelled()` predicate; providers and the loop *poll* it at defined
//! points (before a tool runs, inside the fake's replay loop). It is deliberately
//! **not** tokio's awaitable `CancellationToken`: the whole loop is a cooperative
//! single-task async generator, and every cancellation check in tau is a plain
//! boolean read, so rho models it the same way — a polled predicate behind an
//! `Arc<AtomicBool>` ([`SimpleCancellationToken`]), shared (not awaited).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::BoxFuture;
use futures::stream::BoxStream;

use crate::messages::AgentMessage;
use crate::model_limits::RuntimeModelLimits;
use crate::provider_events::AssistantMessageEvent;
use crate::tools::AgentTool;

/// A polled cancellation predicate (tau `CancellationToken`).
///
/// Checked synchronously at the loop's cancellation points; never awaited.
pub trait CancellationToken: Send + Sync {
    /// Whether the current stream / tool should stop.
    fn is_cancelled(&self) -> bool;
}

/// The boxed assistant-event stream a provider returns (tau's returned
/// `AsyncIterator[AssistantMessageEvent]`).
pub type AssistantEventStream = BoxStream<'static, AssistantMessageEvent>;

/// Provider-neutral Pi-compatible model stream interface (tau `ModelProvider`).
pub trait ModelProvider: Send + Sync {
    /// Stream one model response as assistant message events.
    ///
    /// Synchronous (matching tau): the borrowed `messages`/`tools` must be
    /// snapshotted here if the returned stream needs them, since it is `'static`.
    fn stream_response(
        &self,
        model: &str,
        system: &str,
        messages: &[AgentMessage],
        tools: &[AgentTool],
        signal: Option<Arc<dyn CancellationToken>>,
    ) -> AssistantEventStream;

    /// Discover live per-model limits from the provider's authenticated catalog
    /// (tau's optional `ModelLimitsProvider.discover_model_limits`).
    ///
    /// The default returns `Ok(None)` — a provider that does not advertise a live
    /// catalog is simply not a `ModelLimitsProvider` in tau, so the session falls
    /// back to the static catalog. Only the Codex-subscription adapter overrides
    /// this. Like [`stream_response`](Self::stream_response) the returned future
    /// is `'static`, so an implementer snapshots what it needs synchronously
    /// (the Codex provider is `Arc`-cloneable). `Err` carries a human-readable,
    /// secret-free discovery error for session diagnostics.
    fn discover_model_limits(
        &self,
        model: &str,
    ) -> BoxFuture<'static, Result<Option<RuntimeModelLimits>, String>> {
        let _ = model;
        Box::pin(async { Ok(None) })
    }
}

/// A simple shared cancellation flag (tau `SimpleCancellationToken`).
///
/// `cancel()` flips an `Arc<AtomicBool>`; `is_cancelled()` reads it. Cloning
/// shares the same flag, so the harness can hold one handle while the loop /
/// provider hold another.
#[derive(Debug, Clone, Default)]
pub struct SimpleCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl SimpleCancellationToken {
    /// Build an un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation (idempotent).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl CancellationToken for SimpleCancellationToken {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}
