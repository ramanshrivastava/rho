//! Guest-side runtime: the closure registry, the ergonomic [`Setup`] surface,
//! and the generated [`Guest`](crate::bindings::Guest) dispatcher.
//!
//! Only compiled for `wasm32`. Author-registered closures live in a
//! thread-local registry (WebAssembly is single-threaded, and linear memory
//! persists across export calls, so the registry set up during `init` is intact
//! when the host later dispatches a hook).

use core::cell::RefCell;
use core::marker::PhantomData;

use serde_json::Value;

use crate::bindings::Guest;
use crate::bindings::rho::extension::host;
use crate::bindings::rho::extension::types as wit;
use crate::{
    CommandDef, Extension, InputEvent, InputOutcome, LifecycleEvent, RenderRequest, ToolCallEvent,
    ToolCallOutcome, ToolDef, ToolResult, ToolResultEvent, ToolResultOutcome,
};

type ToolHandler = Box<dyn Fn(Value) -> ToolResult>;
type Renderer = Box<dyn Fn(RenderRequest) -> Option<String>>;
type InputHandler = Box<dyn Fn(InputEvent) -> Option<InputOutcome>>;
type ToolCallHandler = Box<dyn Fn(ToolCallEvent) -> Option<ToolCallOutcome>>;
type ToolResultHandler = Box<dyn Fn(ToolResultEvent) -> Option<ToolResultOutcome>>;
type LifecycleHandler = Box<dyn Fn(LifecycleEvent)>;
type AgentEventHandler = Box<dyn Fn(String, Value)>;

#[derive(Default)]
struct Registry {
    tools: Vec<(String, ToolHandler)>,
    renderers: Vec<(String, Renderer)>,
    on_input: Vec<InputHandler>,
    on_tool_call: Vec<ToolCallHandler>,
    on_tool_result: Vec<ToolResultHandler>,
    on_session_start: Vec<LifecycleHandler>,
    on_session_shutdown: Vec<LifecycleHandler>,
    on_agent_event: Vec<AgentEventHandler>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn to_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

/// The registration surface handed to [`Extension::setup`]. Every method both
/// registers with the host (so the host knows the surface exists and dispatches
/// the matching hook) and stores the author's closure for later dispatch.
pub struct Setup {
    _private: (),
}

impl Setup {
    /// Register a custom tool with its execution closure.
    pub fn tool<F>(&mut self, def: ToolDef, handler: F)
    where
        F: Fn(Value) -> ToolResult + 'static,
    {
        host::register_tool(&wit::ToolDef {
            name: def.name.clone(),
            label: def.label,
            description: def.description,
            parameters_json: to_json(&def.parameters),
            prompt_snippet: def.prompt_snippet,
        });
        REGISTRY.with_borrow_mut(|r| r.tools.push((def.name, Box::new(handler))));
    }

    /// Register a slash command.
    pub fn command(&mut self, def: CommandDef) {
        host::register_command(&wit::CommandDef {
            name: def.name,
            description: def.description,
            usage: def.usage,
            aliases: def.aliases,
        });
    }

    /// Add a standalone system-prompt guideline line.
    pub fn guideline(&mut self, text: impl Into<String>) {
        host::add_prompt_guideline(&text.into());
    }

    /// Register an `input` hook.
    pub fn on_input<F>(&mut self, handler: F)
    where
        F: Fn(InputEvent) -> Option<InputOutcome> + 'static,
    {
        host::subscribe("input");
        REGISTRY.with_borrow_mut(|r| r.on_input.push(Box::new(handler)));
    }

    /// Register a `tool_call` hook.
    pub fn on_tool_call<F>(&mut self, handler: F)
    where
        F: Fn(ToolCallEvent) -> Option<ToolCallOutcome> + 'static,
    {
        host::subscribe("tool_call");
        REGISTRY.with_borrow_mut(|r| r.on_tool_call.push(Box::new(handler)));
    }

    /// Register a `tool_result` hook.
    pub fn on_tool_result<F>(&mut self, handler: F)
    where
        F: Fn(ToolResultEvent) -> Option<ToolResultOutcome> + 'static,
    {
        host::subscribe("tool_result");
        REGISTRY.with_borrow_mut(|r| r.on_tool_result.push(Box::new(handler)));
    }

    /// Register a `session_start` hook.
    pub fn on_session_start<F>(&mut self, handler: F)
    where
        F: Fn(LifecycleEvent) + 'static,
    {
        host::subscribe("session_start");
        REGISTRY.with_borrow_mut(|r| r.on_session_start.push(Box::new(handler)));
    }

    /// Register a `session_shutdown` hook.
    pub fn on_session_shutdown<F>(&mut self, handler: F)
    where
        F: Fn(LifecycleEvent) + 'static,
    {
        host::subscribe("session_shutdown");
        REGISTRY.with_borrow_mut(|r| r.on_session_shutdown.push(Box::new(handler)));
    }

    /// Register a generic agent-event hook (tau's `agent_event` wildcard).
    pub fn on_agent_event<F>(&mut self, handler: F)
    where
        F: Fn(String, Value) + 'static,
    {
        host::subscribe("agent_event");
        REGISTRY.with_borrow_mut(|r| r.on_agent_event.push(Box::new(handler)));
    }

    /// Register a custom-message renderer for `custom_type`.
    pub fn message_renderer<F>(&mut self, custom_type: impl Into<String>, handler: F)
    where
        F: Fn(RenderRequest) -> Option<String> + 'static,
    {
        let custom_type = custom_type.into();
        // First registration per custom_type wins (mirrors the host's contract):
        // keeping later duplicates would let `render_message` fall through to a
        // shadowed handler when the first returns `None`.
        if REGISTRY.with_borrow(|r| r.renderers.iter().any(|(ty, _)| ty == &custom_type)) {
            return;
        }
        host::register_message_renderer(&custom_type);
        REGISTRY.with_borrow_mut(|r| r.renderers.push((custom_type, Box::new(handler))));
    }
}

/// The generic component entry point: [`export_extension!`](crate::export_extension)
/// exports `Exporter<T>` for the author's `T: Extension`.
pub struct Exporter<T>(PhantomData<T>);

impl<T: Extension> Guest for Exporter<T> {
    fn init() {
        let mut setup = Setup { _private: () };
        T::setup(&mut setup);
    }

    fn on_session_start(ev: wit::LifecycleEvent) {
        REGISTRY.with_borrow(|r| {
            for h in &r.on_session_start {
                h(LifecycleEvent {
                    reason: ev.reason.clone(),
                });
            }
        });
    }

    fn on_session_shutdown(ev: wit::LifecycleEvent) {
        REGISTRY.with_borrow(|r| {
            for h in &r.on_session_shutdown {
                h(LifecycleEvent {
                    reason: ev.reason.clone(),
                });
            }
        });
    }

    fn on_agent_event(event_type: String, payload_json: String) {
        let payload = parse_json(&payload_json);
        REGISTRY.with_borrow(|r| {
            for h in &r.on_agent_event {
                h(event_type.clone(), payload.clone());
            }
        });
    }

    fn on_input(ev: wit::InputEvent) -> Option<wit::InputOutcome> {
        let event = InputEvent {
            text: ev.text,
            source: ev.source,
            streaming_behavior: ev.streaming_behavior,
        };
        REGISTRY.with_borrow(|r| {
            for h in &r.on_input {
                if let Some(outcome) = h(event.clone()) {
                    return Some(match outcome {
                        InputOutcome::Transform(text) => wit::InputOutcome {
                            action: "transform".to_string(),
                            text: Some(text),
                            message: None,
                        },
                        InputOutcome::Handled(message) => wit::InputOutcome {
                            action: "handled".to_string(),
                            text: None,
                            message,
                        },
                    });
                }
            }
            None
        })
    }

    fn on_tool_call(ev: wit::ToolCallEvent) -> Option<wit::ToolCallOutcome> {
        let event = ToolCallEvent {
            tool_name: ev.tool_name,
            arguments: parse_json(&ev.arguments_json),
        };
        REGISTRY.with_borrow(|r| {
            for h in &r.on_tool_call {
                if let Some(outcome) = h(event.clone()) {
                    return Some(wit::ToolCallOutcome {
                        block: outcome.block,
                        reason: outcome.reason,
                        arguments_json: outcome.arguments.as_ref().map(to_json),
                    });
                }
            }
            None
        })
    }

    fn on_tool_result(ev: wit::ToolResultEvent) -> Option<wit::ToolResultOutcome> {
        let event = ToolResultEvent {
            tool_name: ev.tool_name,
            arguments: parse_json(&ev.arguments_json),
            result_text: ev.result_text,
            result_details: ev.result_details_json.as_deref().map(parse_json),
        };
        REGISTRY.with_borrow(|r| {
            for h in &r.on_tool_result {
                if let Some(outcome) = h(event.clone()) {
                    return Some(wit::ToolResultOutcome {
                        content: outcome.content,
                        details_json: outcome.details.as_ref().map(to_json),
                    });
                }
            }
            None
        })
    }

    fn call_tool(name: String, arguments_json: String) -> String {
        let args = parse_json(&arguments_json);
        let result = REGISTRY.with_borrow(|r| {
            r.tools
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, h)| h(args.clone()))
        });
        let result = result.unwrap_or_default();
        // Omit `details` entirely when absent (`None`); emit a literal `null`
        // when present-but-null (`Some(Value::Null)`). Collapsing the two would
        // lose a byte-compat distinction the host preserves.
        let mut obj = serde_json::Map::new();
        obj.insert("text".to_string(), Value::String(result.text));
        if let Some(details) = result.details {
            obj.insert("details".to_string(), details);
        }
        Value::Object(obj).to_string()
    }

    fn render_message(
        custom_type: String,
        content: String,
        details_json: Option<String>,
        expanded: bool,
    ) -> Option<String> {
        let request = RenderRequest {
            custom_type: custom_type.clone(),
            content,
            details: details_json.as_deref().map(parse_json),
            expanded,
        };
        REGISTRY.with_borrow(|r| {
            for (ty, h) in &r.renderers {
                if *ty == custom_type {
                    if let Some(markup) = h(request.clone()) {
                        return Some(markup);
                    }
                }
            }
            None
        })
    }
}

/// Export an [`Extension`](crate::Extension) implementation as a rho extension
/// component. Place at the crate root of a `cdylib` guest crate.
#[macro_export]
macro_rules! export_extension {
    ($ty:ty) => {
        // `export!` doesn't accept a generic/turbofish type path, so bind the
        // monomorphized exporter to a plain alias first.
        type __RhoExporter = $crate::Exporter<$ty>;
        $crate::bindings::export!(__RhoExporter with_types_in $crate::bindings);
    };
}
