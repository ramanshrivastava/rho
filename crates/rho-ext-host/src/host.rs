//! The `ExtensionHost` abstraction and its collaborators.
//!
//! This is the codex-lesson seam (see `dev-notes/m7-extension-design.md`): the
//! `CodingSession` integration depends on the [`ExtensionHost`] trait, never on
//! wasmtime. The wasmtime-backed [`crate::wasm::WasmExtensionHost`] is the only
//! implementation M7 ships, but [`NoopExtensionHost`] satisfies the trait with
//! zero wasmtime so default builds stay lean, and test doubles (in `rho-coding`)
//! exercise the tau-parity orchestration without a WASM toolchain.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::payload::{
    AgentHookEvent, InputEvent, InputOutcome, LifecycleEvent, ToolCallEvent, ToolCallOutcome,
    ToolCallResult, ToolResultEvent, ToolResultOutcome,
};

/// A discovered extension ready to instantiate: a stable name and a component
/// (`.wasm`) path (tau's `DiscoveredExtension`, WASM-shaped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSpec {
    /// Stable extension name (first-loaded wins on conflict).
    pub name: String,
    /// Path to the compiled component module.
    pub path: PathBuf,
}

/// The registrations one extension produced during its `init` phase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedExtension {
    /// The extension's name.
    pub name: String,
    /// Tools registered via `register-tool`, in registration order.
    pub tools: Vec<crate::payload::ToolDef>,
    /// Commands registered via `register-command`, in registration order.
    pub commands: Vec<crate::payload::CommandDef>,
    /// Standalone prompt guidelines, in registration order.
    pub guidelines: Vec<String>,
    /// Custom-message types the extension registered a renderer for.
    pub message_renderers: Vec<String>,
    /// Hook/agent-event names the extension subscribed to (tau's `on(...)`).
    pub subscriptions: Vec<String>,
    /// Whether the extension registered a key interceptor.
    pub registers_key_interceptor: bool,
}

/// A non-fatal problem surfaced while loading or running an extension. Mirrors
/// the shape of tau's `ResourceDiagnostic` (rho maps it into that type at the
/// `rho-coding` seam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDiagnostic {
    /// The extension the diagnostic concerns (empty for path-level problems).
    pub extension: String,
    /// The offending path, if any.
    pub path: Option<PathBuf>,
    /// Human-readable message.
    pub message: String,
    /// `true` for errors, `false` for warnings.
    pub is_error: bool,
}

impl HostDiagnostic {
    /// An error-severity diagnostic for `extension`.
    #[must_use]
    pub fn error(extension: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            extension: extension.into(),
            path: None,
            message: message.into(),
            is_error: true,
        }
    }

    /// A warning-severity diagnostic for `extension`.
    #[must_use]
    pub fn warning(extension: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            extension: extension.into(),
            path: None,
            message: message.into(),
            is_error: false,
        }
    }
}

/// The result of loading a set of extensions: what each registered, plus
/// non-fatal diagnostics (per-extension `init` failures are diagnostics, not
/// hard errors — tau's isolation boundary).
#[derive(Debug, Clone, Default)]
pub struct LoadOutcome {
    /// Successfully instantiated extensions, in load order.
    pub extensions: Vec<LoadedExtension>,
    /// Load-time diagnostics.
    pub diagnostics: Vec<HostDiagnostic>,
}

/// An error dispatching to an extension (a trap, a missing extension, a decode
/// failure). Distinct from a *handler-returned* outcome, which is a normal
/// value. The `rho-coding` runtime turns these into runtime diagnostics and
/// applies tau's fail-safe semantics (e.g. a failing `tool_call` hook blocks).
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The named extension is not loaded.
    #[error("unknown extension: {0}")]
    UnknownExtension(String),
    /// The guest trapped, or the host could not drive it.
    #[error("extension `{extension}` {event} failed: {message}")]
    Dispatch {
        /// The extension name.
        extension: String,
        /// The event/entry point that failed.
        event: String,
        /// The underlying message.
        message: String,
    },
}

impl HostError {
    /// Build a [`HostError::Dispatch`].
    #[must_use]
    pub fn dispatch(
        extension: impl Into<String>,
        event: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Dispatch {
            extension: extension.into(),
            event: event.into(),
            message: message.into(),
        }
    }
}

/// Host capabilities a running guest can call back into during hook dispatch:
/// the read-only session context (tau `ExtensionContext`), the UI facade (tau
/// `ExtensionUi`), and message-sending actions. Implemented by the `rho-coding`
/// runtime and injected via [`ExtensionHost::bind`]; the wasmtime host stores it
/// in its component store state and services the `host` WIT imports from it.
///
/// All methods are async so the UI dialogs (tau's `select`/`confirm`/`input`)
/// can await a frontend, matching `func_wrap_async` on the host side.
#[async_trait]
pub trait HostBridge: Send + Sync {
    /// The session working directory.
    async fn cwd(&self) -> String;
    /// The active model name.
    async fn model(&self) -> String;
    /// The active provider name.
    async fn provider_name(&self) -> String;
    /// The current session id, if indexed.
    async fn session_id(&self) -> Option<String>;
    /// The active system prompt.
    async fn system_prompt(&self) -> String;
    /// Whether an agent run is currently active.
    async fn is_running(&self) -> bool;
    /// The active-branch transcript as JSON text (deep-copied host-side).
    async fn transcript_json(&self) -> String;

    /// Show a notification (`level` is `info`/`warning`/`error`).
    async fn notify(&self, message: &str, level: &str);
    /// Show a picker; `None` on cancel or no UI.
    async fn ui_select(&self, title: &str, options: &[String]) -> Option<String>;
    /// Show a confirmation; `true` only if confirmed.
    async fn ui_confirm(&self, title: &str, message: &str) -> bool;
    /// Show a text prompt; `None` on cancel or no UI.
    async fn ui_input(&self, title: &str, placeholder: &str) -> Option<String>;

    /// Queue a user message (`deliver_as` is `steer`/`follow_up`).
    async fn send_user_message(&self, content: &str, deliver_as: &str);
}

/// The extension host abstraction: load extensions, run their hooks, invoke
/// their tools/renderers, and tear down. One instance backs a `CodingSession`'s
/// extension runtime for the process lifetime; `bind`/`load` re-target it on
/// session replacement and `/reload` (tau's long-lived-runtime model).
#[async_trait]
pub trait ExtensionHost: Send + Sync {
    /// Instantiate `specs` and run each `init`, returning what each registered
    /// plus per-extension diagnostics. `bridge` is retained for later hook
    /// dispatch. Replaces any previously loaded set (`/reload` semantics).
    async fn load(&self, specs: &[ExtensionSpec], bridge: Arc<dyn HostBridge>) -> LoadOutcome;

    /// Re-point the host at a fresh [`HostBridge`] without reloading modules
    /// (tau's session rebind: resume/new/branch keep the same registrations).
    async fn bind(&self, bridge: Arc<dyn HostBridge>);

    /// Execute a guest-registered tool.
    async fn call_tool(
        &self,
        extension: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<ToolCallResult, HostError>;

    /// Run one extension's `input` hook.
    async fn on_input(
        &self,
        extension: &str,
        event: &InputEvent,
    ) -> Result<Option<InputOutcome>, HostError>;

    /// Run one extension's `tool_call` hook.
    async fn on_tool_call(
        &self,
        extension: &str,
        event: &ToolCallEvent,
    ) -> Result<Option<ToolCallOutcome>, HostError>;

    /// Run one extension's `tool_result` hook.
    async fn on_tool_result(
        &self,
        extension: &str,
        event: &ToolResultEvent,
    ) -> Result<Option<ToolResultOutcome>, HostError>;

    /// Run one extension's `session_start` hook.
    async fn on_session_start(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError>;

    /// Run one extension's `session_shutdown` hook.
    async fn on_session_shutdown(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError>;

    /// Run one extension's generic `on-agent-event` handler.
    async fn on_agent_event(
        &self,
        extension: &str,
        event: &AgentHookEvent,
    ) -> Result<(), HostError>;

    /// Render a custom message via a guest renderer; `None` falls back to raw.
    async fn render_message(
        &self,
        extension: &str,
        custom_type: &str,
        content: &str,
        details: Option<&Value>,
        expanded: bool,
    ) -> Result<Option<String>, HostError>;

    /// Tear down all instances (drop stores/instances). Idempotent.
    async fn teardown(&self);
}

/// The default host when the `wasmtime` feature is off (or a session has no
/// extensions): loads nothing and dispatches nothing. Every default rho build
/// links this — it pulls in zero WASM machinery, exactly like tau's
/// `NullUiBridge` keeps print mode free of the TUI graph.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopExtensionHost;

#[async_trait]
impl ExtensionHost for NoopExtensionHost {
    async fn load(&self, specs: &[ExtensionSpec], _bridge: Arc<dyn HostBridge>) -> LoadOutcome {
        // A no-op host cannot run WASM. If discovery found components but this
        // build lacks the runtime, surface one diagnostic per spec so the user
        // learns why their extensions are inert rather than silently ignored.
        let diagnostics = specs
            .iter()
            .map(|spec| {
                HostDiagnostic::warning(
                    spec.name.clone(),
                    "extension runtime unavailable (built without the `wasmtime` \
                     feature); component ignored",
                )
            })
            .collect();
        LoadOutcome {
            extensions: Vec::new(),
            diagnostics,
        }
    }

    async fn bind(&self, _bridge: Arc<dyn HostBridge>) {}

    async fn call_tool(
        &self,
        extension: &str,
        _tool: &str,
        _arguments: &Value,
    ) -> Result<ToolCallResult, HostError> {
        Err(HostError::UnknownExtension(extension.to_string()))
    }

    async fn on_input(
        &self,
        _extension: &str,
        _event: &InputEvent,
    ) -> Result<Option<InputOutcome>, HostError> {
        Ok(None)
    }

    async fn on_tool_call(
        &self,
        _extension: &str,
        _event: &ToolCallEvent,
    ) -> Result<Option<ToolCallOutcome>, HostError> {
        Ok(None)
    }

    async fn on_tool_result(
        &self,
        _extension: &str,
        _event: &ToolResultEvent,
    ) -> Result<Option<ToolResultOutcome>, HostError> {
        Ok(None)
    }

    async fn on_session_start(
        &self,
        _extension: &str,
        _event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        Ok(())
    }

    async fn on_session_shutdown(
        &self,
        _extension: &str,
        _event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        Ok(())
    }

    async fn on_agent_event(
        &self,
        _extension: &str,
        _event: &AgentHookEvent,
    ) -> Result<(), HostError> {
        Ok(())
    }

    async fn render_message(
        &self,
        _extension: &str,
        _custom_type: &str,
        _content: &str,
        _details: Option<&Value>,
        _expanded: bool,
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn teardown(&self) {}
}
