//! `rho-ext-api` — the extension guest API surface.
//!
//! An extension author links against this crate, writes idiomatic Rust, and
//! compiles to a `wasm32-wasip2` component implementing the `rho:extension` WIT
//! world (see `wit/rho-extension.wit`). This crate is the Rust counterpart to
//! tau's `tau_coding.extensions.api` module.
//!
//! # Writing an extension
//!
//! Implement [`Extension`] and register tools / hooks with closures during
//! [`Extension::setup`], then hand your type to [`export_extension!`]:
//!
//! ```ignore
//! use rho_ext_api::prelude::*;
//!
//! struct HelloExt;
//!
//! impl Extension for HelloExt {
//!     fn setup(rho: &mut Setup) {
//!         rho.tool(
//!             ToolDef::new("hello", "Greet someone by name.")
//!                 .parameters(json!({"type": "object",
//!                     "properties": {"who": {"type": "string"}}})),
//!             |args| {
//!                 let who = args.get("who").and_then(Value::as_str).unwrap_or("world");
//!                 ToolResult::text(format!("Hello, {who}!"))
//!             },
//!         );
//!     }
//! }
//!
//! export_extension!(HelloExt);
//! ```
//!
//! The generated `Guest` implementation dispatches `init` (running your
//! `setup`), every subscribed hook, `call-tool`, and `render-message` to the
//! closures you registered, and calls the host `subscribe` import for each hook
//! you subscribe to — so the host only dispatches hooks you actually handle
//! (tau's `_handlers_for`).

use serde_json::Value;

/// A custom tool an extension registers during setup (tau `AgentTool`).
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Tool name (unique; first registration per name wins).
    pub name: String,
    /// Display label (defaults to `name`).
    pub label: String,
    /// Model-facing description.
    pub description: String,
    /// JSON-schema object for the tool's parameters.
    pub parameters: Value,
    /// Optional system-prompt snippet contributed by the tool.
    pub prompt_snippet: Option<String>,
}

impl ToolDef {
    /// A tool named `name` with the given model-facing `description`. The label
    /// defaults to `name` and the parameter schema to an empty object.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            label: name.clone(),
            name,
            description: description.into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            prompt_snippet: None,
        }
    }

    /// Set the display label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Set the JSON-schema parameter object.
    #[must_use]
    pub fn parameters(mut self, parameters: Value) -> Self {
        self.parameters = parameters;
        self
    }

    /// Set the system-prompt snippet contributed by the tool.
    #[must_use]
    pub fn prompt_snippet(mut self, snippet: impl Into<String>) -> Self {
        self.prompt_snippet = Some(snippet.into());
        self
    }
}

/// A slash command an extension registers during setup.
#[derive(Debug, Clone)]
pub struct CommandDef {
    /// Command name (without the leading slash).
    pub name: String,
    /// User-facing description.
    pub description: String,
    /// Usage string (defaults to `/<name>` when absent).
    pub usage: Option<String>,
    /// Command aliases.
    pub aliases: Vec<String>,
}

impl CommandDef {
    /// A command named `name` with the given `description`.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            usage: None,
            aliases: Vec::new(),
        }
    }
}

/// The result of running a custom tool (tau `AgentToolResult`).
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    /// The result's text content.
    pub text: String,
    /// Optional structured details.
    pub details: Option<Value>,
}

impl ToolResult {
    /// A text-only result.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            details: None,
        }
    }

    /// Attach structured details.
    #[must_use]
    pub fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// A `tool_call` hook event, before a tool executes (tau `ToolCallHookEvent`).
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    /// The tool about to execute.
    pub tool_name: String,
    /// The tool's arguments.
    pub arguments: Value,
}

/// The outcome an `on_tool_call` hook returns (tau `ToolCallHookResult`).
#[derive(Debug, Clone, Default)]
pub struct ToolCallOutcome {
    /// Block execution; the model sees `reason` instead.
    pub block: bool,
    /// Optional block reason.
    pub reason: Option<String>,
    /// Replacement arguments; `None` leaves them unchanged.
    pub arguments: Option<Value>,
}

impl ToolCallOutcome {
    /// Block the tool call, surfacing `reason` to the model.
    #[must_use]
    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            block: true,
            reason: Some(reason.into()),
            arguments: None,
        }
    }

    /// Allow the tool call but rewrite its arguments.
    #[must_use]
    pub fn rewrite(arguments: Value) -> Self {
        Self {
            block: false,
            reason: None,
            arguments: Some(arguments),
        }
    }
}

/// A `tool_result` hook event, after a tool executes (tau `ToolResultHookEvent`).
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    /// The tool that executed.
    pub tool_name: String,
    /// The (possibly rewritten) arguments the tool ran with.
    pub arguments: Value,
    /// The result's text content.
    pub result_text: String,
    /// The result's structured details, if any.
    pub result_details: Option<Value>,
}

/// The outcome an `on_tool_result` hook returns (tau `ToolResultHookResult`).
#[derive(Debug, Clone, Default)]
pub struct ToolResultOutcome {
    /// Override the result's text content.
    pub content: Option<String>,
    /// Override the result's structured details.
    pub details: Option<Value>,
}

/// An `input` hook event: raw prompt text before expansion (tau `InputEvent`).
#[derive(Debug, Clone)]
pub struct InputEvent {
    /// Raw prompt text.
    pub text: String,
    /// `"interactive"` or `"extension"`.
    pub source: String,
    /// `"steer"` / `"follow_up"`, or `None` on the idle prompt path.
    pub streaming_behavior: Option<String>,
}

/// The outcome an `on_input` hook returns; return `None` to leave input as-is.
#[derive(Debug, Clone)]
pub enum InputOutcome {
    /// Replace the prompt text (transforms chain across handlers).
    Transform(String),
    /// Consume the input entirely, optionally showing `message` to the user.
    Handled(Option<String>),
}

/// A lifecycle event (`session_start` / `session_shutdown`).
#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    /// `"startup" | "reload" | "new" | "resume" | "branch" | "quit"`.
    pub reason: String,
}

/// A request to render a custom message via a registered renderer.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// The custom message type.
    pub custom_type: String,
    /// The message's content.
    pub content: String,
    /// The message's structured details, if any.
    pub details: Option<Value>,
    /// Whether the message is expanded.
    pub expanded: bool,
}

/// An extension: register tools, commands, guidelines, renderers, and hook
/// handlers during [`setup`](Extension::setup). Hand the type to
/// [`export_extension!`] to make it a component entry point.
pub trait Extension {
    /// Register the extension's surfaces and hook handlers. Runs once, during
    /// the guest's `init` phase (registration is honored only here).
    fn setup(rho: &mut Setup);
}

/// The convenient imports for writing an extension guest.
pub mod prelude {
    pub use crate::{
        CommandDef, Extension, InputEvent, InputOutcome, LifecycleEvent, RenderRequest, Setup,
        ToolCallEvent, ToolCallOutcome, ToolDef, ToolResult, ToolResultEvent, ToolResultOutcome,
    };
    pub use serde_json::{Value, json};

    // The `export_extension!` macro is only defined when compiling to
    // WebAssembly (its expansion invokes the wit-bindgen `export!` machinery).
    #[cfg(target_arch = "wasm32")]
    pub use crate::export_extension;
}

// ===========================================================================
// Guest runtime — only meaningful when compiling to WebAssembly. A host build
// of this crate links none of it.
// ===========================================================================

#[cfg(target_arch = "wasm32")]
pub mod bindings {
    wit_bindgen::generate!({
        world: "extension",
        path: "wit/rho-extension.wit",
        pub_export_macro: true,
        default_bindings_module: "rho_ext_api::bindings",
        export_macro_name: "export",
    });
}

#[cfg(target_arch = "wasm32")]
mod runtime;

#[cfg(target_arch = "wasm32")]
pub use runtime::{Exporter, Setup};

/// A `Setup` handle (host build stub — the real registration surface exists only
/// on `wasm32`, where extensions are compiled).
#[cfg(not(target_arch = "wasm32"))]
pub struct Setup {
    _private: (),
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::missing_const_for_fn, missing_docs)]
impl Setup {
    /// Register a custom tool (host-build stub).
    pub fn tool<F: Fn(Value) -> ToolResult>(&mut self, _def: ToolDef, _handler: F) {}
    /// Register a slash command (host-build stub).
    pub fn command(&mut self, _def: CommandDef) {}
    /// Add a standalone system-prompt guideline (host-build stub).
    pub fn guideline(&mut self, _text: impl Into<String>) {}
    /// Register an `input` hook (host-build stub).
    pub fn on_input<F: Fn(InputEvent) -> Option<InputOutcome>>(&mut self, _f: F) {}
    /// Register a `tool_call` hook (host-build stub).
    pub fn on_tool_call<F: Fn(ToolCallEvent) -> Option<ToolCallOutcome>>(&mut self, _f: F) {}
    /// Register a `tool_result` hook (host-build stub).
    pub fn on_tool_result<F: Fn(ToolResultEvent) -> Option<ToolResultOutcome>>(&mut self, _f: F) {}
    /// Register a `session_start` hook (host-build stub).
    pub fn on_session_start<F: Fn(LifecycleEvent)>(&mut self, _f: F) {}
    /// Register a `session_shutdown` hook (host-build stub).
    pub fn on_session_shutdown<F: Fn(LifecycleEvent)>(&mut self, _f: F) {}
    /// Register a generic agent-event hook (host-build stub).
    pub fn on_agent_event<F: Fn(String, Value)>(&mut self, _f: F) {}
    /// Register a custom-message renderer (host-build stub).
    pub fn message_renderer<F: Fn(RenderRequest) -> Option<String>>(
        &mut self,
        _custom_type: impl Into<String>,
        _f: F,
    ) {
    }
}
