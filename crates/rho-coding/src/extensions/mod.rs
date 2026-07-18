//! Extension runtime: the tau-parity orchestration layered on the frozen
//! [`rho_ext_host::ExtensionHost`] trait.
//!
//! Port of tau's `tau_coding/extensions/runtime.py`. The split follows the
//! codex-lesson seam from `dev-notes/m7-extension-design.md`: `rho-ext-host`
//! owns the transport (WASM component instances, hook dispatch to guests); this
//! module owns the *semantics* tau's `ExtensionRuntime` defines — first-wins
//! registration, tool composition + the `tool_call`/`tool_result` hook seam,
//! prompt-guideline collection, the input-hook chain, the canonical agent-event
//! fan-out with Pi turn-index adaptation, message-renderer resolution, and
//! diagnostics.
//!
//! The runtime is generic over the host, defaulting to
//! [`rho_ext_host::NoopExtensionHost`] so a default rho build links zero WASM
//! machinery and behaves exactly as before (no extensions → no behavior change).
//! Tests drive the full orchestration through [`fake_host::FakeExtensionHost`],
//! an in-memory host that registers tools/hooks as Rust closures.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde_json::Value;

use rho_agent::events::AgentEvent;
use rho_agent::messages::{TextContent, ToolResultContent};
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolExecutor};
use rho_agent::types::JsonMap;

use rho_ext_host::{
    AgentHookEvent, CommandDef, DiscoveryPaths, ExtensionHost, ExtensionSpec, HostBridge,
    HostDiagnostic, HostError, InputAction, InputEvent, LifecycleEvent, LoadedExtension,
    NoopExtensionHost, ToolCallEvent, ToolCallOutcome, ToolCallResult, ToolDef, ToolResultEvent,
    ToolResultOutcome, discover_extensions,
};

use crate::commands::{
    CommandContext, CommandRegistry, CommandResult, SlashCommand, create_default_command_registry,
};
use crate::resources::{ResourceDiagnostic, RhoResourcePaths};

#[cfg(test)]
pub mod fake_host;
#[cfg(test)]
mod tests;

/// Combined outcome of running all `input` hooks over prompt text (tau
/// `InputHookOutcome`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputHookOutcome {
    /// An `input` hook consumed the prompt; no agent run should follow.
    pub handled: bool,
    /// The (possibly transformed) prompt text.
    pub text: String,
    /// An optional message to show the user (set when `handled`).
    pub message: Option<String>,
}

/// A no-op [`HostBridge`]: the default host-capability facade when no session is
/// bound (tau's `NullUiBridge`, expressed as the read-only session/UI seam).
///
/// The `NoopExtensionHost` never invokes a bridge, so this is what a default
/// runtime carries until a real session installs one.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHostBridge;

#[async_trait::async_trait]
impl HostBridge for NoopHostBridge {
    async fn cwd(&self) -> String {
        String::new()
    }
    async fn model(&self) -> String {
        String::new()
    }
    async fn provider_name(&self) -> String {
        String::new()
    }
    async fn session_id(&self) -> Option<String> {
        None
    }
    async fn system_prompt(&self) -> String {
        String::new()
    }
    async fn is_running(&self) -> bool {
        false
    }
    async fn transcript_json(&self) -> String {
        "[]".to_string()
    }
    async fn notify(&self, _message: &str, _level: &str) {}
    async fn ui_select(&self, _title: &str, _options: &[String]) -> Option<String> {
        None
    }
    async fn ui_confirm(&self, _title: &str, _message: &str) -> bool {
        false
    }
    async fn ui_input(&self, _title: &str, _placeholder: &str) -> Option<String> {
        None
    }
    async fn send_user_message(&self, _content: &str, _deliver_as: &str) {}
}

/// The live, read-only session context a bound session shares with its
/// extensions (tau's `ExtensionContext` fields). Held behind an
/// `Arc<Mutex<_>>` so the session can update it in place (on `/model`,
/// `/login`, reload) and a running extension reads the current value.
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    /// Session working directory.
    pub cwd: String,
    /// Active model name.
    pub model: String,
    /// Active provider name.
    pub provider_name: String,
    /// Current session id, if indexed.
    pub session_id: Option<String>,
    /// Active system prompt.
    pub system_prompt: String,
}

/// A [`HostBridge`] backed by a live [`SessionContext`] snapshot. Reads reflect
/// the current session (`cwd`/`model`/`provider`/`session_id`/`system_prompt`),
/// so extension `context.*` reads work in both print mode and the TUI.
///
/// `transcript_json`/`is_running` and the UI dialogs remain the no-op defaults:
/// the transcript and run-state live inside the (non-`Arc`) `AgentHarness`, and
/// wiring live UI dialogs needs the frontend handle threaded through — both are
/// tracked follow-ups (see `dev-notes/phase-7.md`). The two shipped example
/// guests read neither, so this fully serves them.
#[derive(Debug, Clone)]
pub struct SessionContextBridge {
    context: Arc<Mutex<SessionContext>>,
}

impl SessionContextBridge {
    /// Build a bridge over a shared session context.
    #[must_use]
    pub fn new(context: Arc<Mutex<SessionContext>>) -> Self {
        Self { context }
    }
}

#[async_trait::async_trait]
impl HostBridge for SessionContextBridge {
    async fn cwd(&self) -> String {
        self.context.lock().unwrap().cwd.clone()
    }
    async fn model(&self) -> String {
        self.context.lock().unwrap().model.clone()
    }
    async fn provider_name(&self) -> String {
        self.context.lock().unwrap().provider_name.clone()
    }
    async fn session_id(&self) -> Option<String> {
        self.context.lock().unwrap().session_id.clone()
    }
    async fn system_prompt(&self) -> String {
        self.context.lock().unwrap().system_prompt.clone()
    }
    async fn is_running(&self) -> bool {
        false
    }
    async fn transcript_json(&self) -> String {
        "[]".to_string()
    }
    async fn notify(&self, _message: &str, _level: &str) {}
    async fn ui_select(&self, _title: &str, _options: &[String]) -> Option<String> {
        None
    }
    async fn ui_confirm(&self, _title: &str, _message: &str) -> bool {
        false
    }
    async fn ui_input(&self, _title: &str, _placeholder: &str) -> Option<String> {
        None
    }
    async fn send_user_message(&self, _content: &str, _deliver_as: &str) {}
}

/// One loaded extension's dispatch metadata (name + which events it subscribed
/// to). Kept in load order so hook dispatch is deterministic (tau's
/// `_handlers_for` yields in load order).
#[derive(Debug, Clone)]
struct ExtMeta {
    name: String,
    subscriptions: HashSet<String>,
}

impl ExtMeta {
    fn subscribes(&self, event: &str) -> bool {
        self.subscriptions.contains(event)
    }
}

/// A tool registered by an extension, with the owning extension name (tau
/// `RegisteredExtensionTool`).
#[derive(Clone)]
struct RegisteredExtensionTool {
    extension: String,
    tool: AgentTool,
}

/// A slash command registered by an extension (tau `ExtensionCommand`, minus the
/// guest handler — see the module note on command execution).
#[derive(Debug, Clone)]
struct ExtensionCommand {
    extension: String,
    name: String,
    description: String,
    usage: String,
    aliases: Vec<String>,
}

/// The wildcard agent-event subscription name (tau `AGENT_EVENT_WILDCARD`).
const AGENT_EVENT_WILDCARD: &str = "agent_event";

/// The shared, `Arc`-clonable dispatch core.
///
/// Detached tool executors (produced by [`ExtensionRuntime::compose_tools`])
/// outlive the borrow of the runtime, so everything a hook touches at dispatch
/// time — the host, the per-extension subscription table, the runtime
/// diagnostics sink, and the renderer failure-dedup set — lives here behind an
/// `Arc`. Rebuilt on every `load`/reset so a reload starts from a clean slate.
struct RuntimeShared {
    host: Arc<dyn ExtensionHost>,
    extensions: Vec<ExtMeta>,
    /// `custom_type` → owning extension (first registration wins).
    message_renderers: HashMap<String, String>,
    diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    renderer_failures: Mutex<HashSet<String>>,
}

impl RuntimeShared {
    fn record_runtime_failure(&self, extension: &str, event: &str, message: &str) {
        self.diagnostics.lock().expect("diagnostics lock").push(
            ResourceDiagnostic::new(
                "extension",
                format!("handler for `{event}` raised: {message}"),
            )
            .with_name(extension.to_string())
            .with_severity("error"),
        );
    }

    fn subscribers<'a>(&'a self, event: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.extensions
            .iter()
            .filter(move |ext| ext.subscribes(event))
            .map(|ext| ext.name.as_str())
    }

    /// tau `_run_tool_call_hooks`: sequential per subscribed extension; blocking
    /// wins and short-circuits; argument rewrites chain; a raising/erroring hook
    /// is fail-safe (blocks).
    async fn run_tool_call_hooks(&self, tool_name: &str, arguments: &JsonMap) -> ToolCallOutcome {
        let mut effective = arguments.clone();
        let mut changed = false;
        for extension in self
            .subscribers("tool_call")
            .map(str::to_string)
            .collect::<Vec<_>>()
        {
            let event = ToolCallEvent {
                tool_name: tool_name.to_string(),
                arguments: Value::Object(effective.clone()),
            };
            match self.host.on_tool_call(&extension, &event).await {
                Err(err) => {
                    self.record_runtime_failure(&extension, "tool_call", &host_error_message(&err));
                    return ToolCallOutcome {
                        block: true,
                        reason: Some(format!(
                            "extension `{extension}` tool_call hook failed: {}",
                            host_error_message(&err)
                        )),
                        arguments: None,
                    };
                }
                Ok(None) => {}
                Ok(Some(outcome)) => {
                    if outcome.block {
                        return ToolCallOutcome {
                            block: true,
                            reason: outcome.reason,
                            arguments: None,
                        };
                    }
                    if let Some(new_arguments) = outcome.arguments {
                        effective = value_to_map(new_arguments);
                        changed = true;
                    }
                }
            }
        }
        ToolCallOutcome {
            block: false,
            reason: None,
            arguments: changed.then(|| Value::Object(effective)),
        }
    }

    /// tau `_run_tool_result_hooks`: content/details override chain; a raising
    /// hook keeps the current result and records a diagnostic.
    async fn run_tool_result_hooks(
        &self,
        tool_name: &str,
        arguments: &JsonMap,
        mut current: AgentToolResult,
    ) -> AgentToolResult {
        for extension in self
            .subscribers("tool_result")
            .map(str::to_string)
            .collect::<Vec<_>>()
        {
            let event = ToolResultEvent {
                tool_name: tool_name.to_string(),
                arguments: Value::Object(arguments.clone()),
                result_text: current.text(),
                result_details: current.details.clone(),
            };
            match self.host.on_tool_result(&extension, &event).await {
                Err(err) => {
                    self.record_runtime_failure(
                        &extension,
                        "tool_result",
                        &host_error_message(&err),
                    );
                }
                Ok(None) => {}
                Ok(Some(ToolResultOutcome { content, details })) => {
                    if let Some(content) = content {
                        current.content = vec![ToolResultContent::Text(TextContent::new(content))];
                    }
                    if let Some(details) = details {
                        current.details = Some(details);
                    }
                }
            }
        }
        current
    }

    /// tau `run_input_hooks`: transforms chain, `handled` short-circuits, a
    /// raising hook records a diagnostic and is otherwise ignored (not
    /// fail-safe, unlike `tool_call`).
    async fn run_input_hooks(
        &self,
        text: &str,
        source: &str,
        streaming_behavior: Option<String>,
    ) -> InputHookOutcome {
        let mut current = text.to_string();
        for extension in self
            .subscribers("input")
            .map(str::to_string)
            .collect::<Vec<_>>()
        {
            let event = InputEvent {
                text: current.clone(),
                source: source.to_string(),
                streaming_behavior: streaming_behavior.clone(),
            };
            match self.host.on_input(&extension, &event).await {
                Err(err) => {
                    self.record_runtime_failure(&extension, "input", &host_error_message(&err));
                }
                Ok(None) => {}
                Ok(Some(outcome)) => match outcome.action {
                    InputAction::Handled => {
                        return InputHookOutcome {
                            handled: true,
                            text: current,
                            message: outcome.message,
                        };
                    }
                    InputAction::Transform => {
                        if let Some(new_text) = outcome.text {
                            current = new_text;
                        }
                    }
                    InputAction::Continue => {}
                },
            }
        }
        InputHookOutcome {
            handled: false,
            text: current,
            message: None,
        }
    }

    /// tau `emit_event`: dispatch one canonical agent event to per-type
    /// subscribers and wildcard (`agent_event`) subscribers. One dispatch per
    /// subscribing extension (the guest routes internally to its own handlers).
    async fn emit_event(&self, event_type: &str, payload: Value) {
        for extension in self
            .extensions
            .iter()
            .filter(|ext| ext.subscribes(event_type) || ext.subscribes(AGENT_EVENT_WILDCARD))
            .map(|ext| ext.name.clone())
            .collect::<Vec<_>>()
        {
            let event = AgentHookEvent {
                event_type: event_type.to_string(),
                payload: payload.clone(),
            };
            if let Err(err) = self.host.on_agent_event(&extension, &event).await {
                self.record_runtime_failure(&extension, event_type, &host_error_message(&err));
            }
        }
    }

    async fn emit_lifecycle(&self, event: &str, reason: &str) {
        let payload = LifecycleEvent {
            reason: reason.to_string(),
        };
        for extension in self
            .subscribers(event)
            .map(str::to_string)
            .collect::<Vec<_>>()
        {
            let result = if event == "session_start" {
                self.host.on_session_start(&extension, &payload).await
            } else {
                self.host.on_session_shutdown(&extension, &payload).await
            };
            if let Err(err) = result {
                self.record_runtime_failure(&extension, event, &host_error_message(&err));
            }
        }
    }

    /// tau `render_custom_message`: first-registered renderer wins; a missing
    /// renderer or a failing one yields `None` (raw fallback). Failures are
    /// diagnosed once per `custom_type`.
    async fn render_custom_message(
        &self,
        custom_type: &str,
        content: &str,
        details: Option<&Value>,
        expanded: bool,
    ) -> Option<String> {
        let extension = self.message_renderers.get(custom_type)?.clone();
        match self
            .host
            .render_message(&extension, custom_type, content, details, expanded)
            .await
        {
            Ok(markup) => markup,
            Err(err) => {
                let key = format!("message_renderer:{custom_type}");
                let mut reported = self.renderer_failures.lock().expect("failures lock");
                if reported.insert(key.clone()) {
                    self.record_runtime_failure(&extension, &key, &host_error_message(&err));
                }
                None
            }
        }
    }
}

/// Owns loaded extensions and dispatches events between them and a session (tau
/// `ExtensionRuntime`).
pub struct ExtensionRuntime {
    host: Arc<dyn ExtensionHost>,
    bridge: Arc<dyn HostBridge>,
    shared: Arc<RuntimeShared>,
    extensions: Vec<ExtMeta>,
    tools: IndexMap<String, RegisteredExtensionTool>,
    commands: IndexMap<String, ExtensionCommand>,
    prompt_guidelines: Vec<(String, String)>,
    message_renderers: IndexMap<String, String>,
    load_diagnostics: Vec<ResourceDiagnostic>,
    turn_index: u32,
}

impl Default for ExtensionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ExtensionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRuntime")
            .field("extensions", &self.extension_names())
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("commands", &self.commands.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ExtensionRuntime {
    /// A default runtime backed by [`NoopExtensionHost`] (zero WASM machinery).
    #[must_use]
    pub fn new() -> Self {
        Self::with_host(Arc::new(NoopExtensionHost))
    }

    /// The runtime a live session uses: backed by the real
    /// [`WasmExtensionHost`](rho_ext_host::wasm::WasmExtensionHost) when the
    /// `wasmtime` feature is on, and by [`NoopExtensionHost`] otherwise.
    ///
    /// When the wasmtime engine cannot be constructed (a build/platform
    /// problem), this falls back to the no-op host rather than failing session
    /// startup — extensions become inert, which the diagnostics surface.
    #[must_use]
    pub fn for_session() -> Self {
        #[cfg(feature = "wasmtime")]
        {
            match rho_ext_host::wasm::WasmExtensionHost::new() {
                Ok(host) => Self::with_host(Arc::new(host)),
                Err(_) => Self::with_host(Arc::new(NoopExtensionHost)),
            }
        }
        #[cfg(not(feature = "wasmtime"))]
        {
            Self::with_host(Arc::new(NoopExtensionHost))
        }
    }

    /// A runtime backed by an explicit host (the wasmtime host, or a test double).
    #[must_use]
    pub fn with_host(host: Arc<dyn ExtensionHost>) -> Self {
        let bridge: Arc<dyn HostBridge> = Arc::new(NoopHostBridge);
        let shared = Arc::new(RuntimeShared {
            host: host.clone(),
            extensions: Vec::new(),
            message_renderers: HashMap::new(),
            diagnostics: Mutex::new(Vec::new()),
            renderer_failures: Mutex::new(HashSet::new()),
        });
        Self {
            host,
            bridge,
            shared,
            extensions: Vec::new(),
            tools: IndexMap::new(),
            commands: IndexMap::new(),
            prompt_guidelines: Vec::new(),
            message_renderers: IndexMap::new(),
            load_diagnostics: Vec::new(),
            turn_index: 0,
        }
    }

    /// Install the host-capability bridge passed to the host on `load`
    /// (tau's `bind` + `set_ui_bridge`). The bridge is retained by the host for
    /// hook dispatch.
    pub fn set_bridge(&mut self, bridge: Arc<dyn HostBridge>) {
        self.bridge = bridge;
    }

    /// Re-point the host at the current bridge without reloading modules (tau's
    /// session rebind: resume/new/branch keep the same registrations).
    pub async fn rebind(&self) {
        self.host.bind(self.bridge.clone()).await;
    }

    // -- loading -----------------------------------------------------------

    /// Discover extensions under `paths` (+ explicit `extra_paths`) and load
    /// them, recording each registration with tau's first-wins semantics.
    pub async fn load(
        &mut self,
        paths: &RhoResourcePaths,
        extra_paths: &[PathBuf],
        include_resource_dirs: bool,
        include_project_dir: bool,
    ) {
        let discovery = DiscoveryPaths {
            root: paths.root.clone(),
            cwd: paths.cwd.clone(),
        };
        let (specs, diagnostics) = discover_extensions(
            &discovery,
            extra_paths,
            include_resource_dirs,
            include_project_dir,
        );
        self.load_diagnostics
            .extend(diagnostics.iter().map(host_diagnostic_to_resource));
        self.load_discovered(specs).await;
    }

    /// Load an explicit set of discovered specs (the discovery-free seam used by
    /// the session's own discovery and by tests). Applies each extension's
    /// registrations in load order.
    pub async fn load_discovered(&mut self, specs: Vec<ExtensionSpec>) {
        let outcome = self.host.load(&specs, self.bridge.clone()).await;
        self.load_diagnostics
            .extend(outcome.diagnostics.iter().map(host_diagnostic_to_resource));
        for extension in outcome.extensions {
            self.register_extension(&extension);
        }
        self.rebuild_shared();
    }

    fn register_extension(&mut self, extension: &LoadedExtension) {
        self.extensions.push(ExtMeta {
            name: extension.name.clone(),
            subscriptions: extension.subscriptions.iter().cloned().collect(),
        });
        for tool in &extension.tools {
            self.register_tool(&extension.name, tool);
        }
        for command in &extension.commands {
            self.register_command(&extension.name, command);
        }
        for guideline in &extension.guidelines {
            self.register_prompt_guideline(&extension.name, guideline);
        }
        for custom_type in &extension.message_renderers {
            self.register_message_renderer(&extension.name, custom_type);
        }
    }

    fn register_tool(&mut self, extension: &str, tool: &ToolDef) {
        if let Some(existing) = self.tools.get(&tool.name) {
            self.load_diagnostics.push(
                ResourceDiagnostic::new(
                    "extension",
                    format!(
                        "tool `{}` already registered by extension `{}`; ignoring duplicate",
                        tool.name, existing.extension
                    ),
                )
                .with_name(extension.to_string()),
            );
            return;
        }
        let agent_tool = build_extension_tool(self.host.clone(), extension, tool);
        self.tools.insert(
            tool.name.clone(),
            RegisteredExtensionTool {
                extension: extension.to_string(),
                tool: agent_tool,
            },
        );
    }

    fn register_command(&mut self, extension: &str, command: &CommandDef) {
        let normalized = command.name.trim().trim_start_matches('/').to_lowercase();
        if let Some(existing) = self.commands.get(&normalized) {
            self.load_diagnostics.push(
                ResourceDiagnostic::new(
                    "extension",
                    format!(
                        "command `/{normalized}` already registered by extension `{}`; ignoring duplicate",
                        existing.extension
                    ),
                )
                .with_name(extension.to_string()),
            );
            return;
        }
        self.commands.insert(
            normalized.clone(),
            ExtensionCommand {
                extension: extension.to_string(),
                name: normalized.clone(),
                description: command.description.clone(),
                usage: command
                    .usage
                    .clone()
                    .unwrap_or_else(|| format!("/{normalized}")),
                aliases: command.aliases.clone(),
            },
        );
    }

    fn register_prompt_guideline(&mut self, extension: &str, guideline: &str) {
        let normalized = guideline.trim();
        if normalized.is_empty() {
            self.load_diagnostics.push(
                ResourceDiagnostic::new("extension", "empty prompt guideline ignored")
                    .with_name(extension.to_string()),
            );
            return;
        }
        self.prompt_guidelines
            .push((extension.to_string(), normalized.to_string()));
    }

    fn register_message_renderer(&mut self, extension: &str, custom_type: &str) {
        let normalized = custom_type.trim();
        if normalized.is_empty() {
            self.load_diagnostics.push(
                ResourceDiagnostic::new(
                    "extension",
                    "empty custom_type for message renderer ignored",
                )
                .with_name(extension.to_string()),
            );
            return;
        }
        if let Some(existing) = self.message_renderers.get(normalized) {
            self.load_diagnostics.push(
                ResourceDiagnostic::new(
                    "extension",
                    format!(
                        "message renderer for `{normalized}` already registered by extension `{existing}`; ignoring duplicate"
                    ),
                )
                .with_name(extension.to_string()),
            );
            return;
        }
        self.message_renderers
            .insert(normalized.to_string(), extension.to_string());
    }

    fn rebuild_shared(&mut self) {
        self.shared = Arc::new(RuntimeShared {
            host: self.host.clone(),
            extensions: self.extensions.clone(),
            message_renderers: self
                .message_renderers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            diagnostics: Mutex::new(Vec::new()),
            renderer_failures: Mutex::new(HashSet::new()),
        });
    }

    // -- reload ------------------------------------------------------------

    /// Drop all registrations and tear the host down ahead of a re-load (tau
    /// `reset_for_reload`). Full stale-handle invalidation of a pre-reload guest
    /// API object is not reachable through the trait (the guest's API lives
    /// inside the instance, dropped by `teardown`); see the crate report.
    pub async fn reset_for_reload(&mut self) {
        self.host.teardown().await;
        self.extensions.clear();
        self.tools.clear();
        self.commands.clear();
        self.prompt_guidelines.clear();
        self.message_renderers.clear();
        self.load_diagnostics.clear();
        self.turn_index = 0;
        self.rebuild_shared();
    }

    // -- accessors ---------------------------------------------------------

    /// Loaded extension names, in load order.
    #[must_use]
    pub fn extension_names(&self) -> Vec<String> {
        self.extensions.iter().map(|ext| ext.name.clone()).collect()
    }

    /// Whether any extension is loaded (the fast-path gate for `compose_tools`).
    #[must_use]
    pub fn has_extensions(&self) -> bool {
        !self.extensions.is_empty()
    }

    /// Extension-registered tools in registration order (unwrapped).
    #[must_use]
    pub fn extension_tools(&self) -> Vec<AgentTool> {
        self.tools.values().map(|reg| reg.tool.clone()).collect()
    }

    /// Standalone prompt guideline lines, in registration order (tau
    /// `prompt_guidelines`). Fed into the system-prompt builder as
    /// `extra_guidelines`.
    #[must_use]
    pub fn prompt_guidelines(&self) -> Vec<String> {
        self.prompt_guidelines
            .iter()
            .map(|(_, guideline)| guideline.clone())
            .collect()
    }

    /// Load-time and runtime diagnostics, mapped into [`ResourceDiagnostic`]
    /// with kind `"extension"` (tau `diagnostics`).
    #[must_use]
    pub fn diagnostics(&self) -> Vec<ResourceDiagnostic> {
        let mut diagnostics = self.load_diagnostics.clone();
        diagnostics.extend(
            self.shared
                .diagnostics
                .lock()
                .expect("diagnostics lock")
                .iter()
                .cloned(),
        );
        diagnostics
    }

    // -- tools -------------------------------------------------------------

    /// Merge extension tools over built-ins by name (override in place;
    /// extension-only appended in registration order), then wrap every tool in
    /// the `tool_call`/`tool_result` hook seam (tau `compose_tools`).
    ///
    /// Fast path: with no extensions loaded the built-ins are returned unwrapped
    /// — the hook seam is a pure pass-through with no subscribers, so this keeps
    /// default (extension-free) sessions byte-for-byte unchanged.
    #[must_use]
    pub fn compose_tools(&self, builtin_tools: Vec<AgentTool>) -> Vec<AgentTool> {
        if self.extensions.is_empty() {
            return builtin_tools;
        }
        let mut extension_tools: IndexMap<String, AgentTool> = self
            .tools
            .iter()
            .map(|(name, reg)| (name.clone(), reg.tool.clone()))
            .collect();
        let mut merged: Vec<AgentTool> = Vec::with_capacity(builtin_tools.len());
        for tool in builtin_tools {
            match extension_tools.shift_remove(&tool.name) {
                Some(override_tool) => merged.push(override_tool),
                None => merged.push(tool),
            }
        }
        merged.extend(extension_tools.into_values());
        merged
            .into_iter()
            .map(|tool| wrap_tool(self.shared.clone(), tool))
            .collect()
    }

    // -- commands ----------------------------------------------------------

    /// Build a session command registry: defaults plus extension commands
    /// (first-wins; shadowing a built-in is rejected with a diagnostic — tau
    /// `build_command_registry`).
    ///
    /// Execution note: a WASM slash command has no invocation path in the frozen
    /// WIT (no `call-command` export) and rho's `CommandHandler` is a bare `fn`
    /// pointer that cannot carry per-extension state, so the registered handler
    /// reports that the command is not executable in this build. The
    /// registration/layering/shadow semantics are faithful; execution is
    /// deferred (see the crate report's `call-command` concern).
    #[must_use]
    pub fn build_command_registry(&self) -> CommandRegistry {
        let mut registry = create_default_command_registry();
        for command in self.commands.values() {
            let description = if command.description.is_empty() {
                format!("Extension command ({}).", command.extension)
            } else {
                command.description.clone()
            };
            let slash = SlashCommand::new(
                &command.name,
                &command.usage,
                &description,
                extension_command_placeholder,
            )
            .aliases(
                &command
                    .aliases
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .search_terms(&[command.extension.as_str(), "extension"]);
            if let Err(err) = registry.register(slash) {
                self.load_diagnostics_push(
                    ResourceDiagnostic::new(
                        "extension",
                        format!("could not register command `/{}`: {err}", command.name),
                    )
                    .with_name(command.extension.clone()),
                );
            }
        }
        registry
    }

    // build_command_registry takes &self but must record a diagnostic; the
    // load-diagnostics vec is only mutated here through interior sharing with
    // the shared runtime diagnostics sink so the &self signature (matching tau)
    // holds. Route command-registration failures to the runtime diagnostics.
    fn load_diagnostics_push(&self, diagnostic: ResourceDiagnostic) {
        let mut diagnostics = self.shared.diagnostics.lock().expect("diagnostics lock");
        // `build_command_registry` re-runs on every `handle_command`; without
        // this guard an unresolvable command conflict would append the identical
        // diagnostic on each invocation, growing without bound. Distinct
        // conflicts still record (they differ in message/name).
        if diagnostics.contains(&diagnostic) {
            return;
        }
        diagnostics.push(diagnostic);
    }

    // -- hook dispatch (delegated to the shared core) ----------------------

    /// Run `input` hooks over prompt text (tau `run_input_hooks`).
    pub async fn run_input_hooks(
        &self,
        text: &str,
        source: &str,
        streaming_behavior: Option<String>,
    ) -> InputHookOutcome {
        self.shared
            .run_input_hooks(text, source, streaming_behavior)
            .await
    }

    /// Show a notification through the installed host bridge (tau `notify`).
    /// A no-op without an interactive UI bridge.
    pub async fn notify(&self, message: &str, level: &str) {
        self.bridge.notify(message, level).await;
    }

    /// Dispatch `session_start` to subscribed extensions.
    pub async fn emit_session_start(&self, reason: &str) {
        self.shared.emit_lifecycle("session_start", reason).await;
    }

    /// Dispatch `session_shutdown` to subscribed extensions.
    pub async fn emit_session_shutdown(&self, reason: &str) {
        self.shared.emit_lifecycle("session_shutdown", reason).await;
    }

    /// Dispatch one raw canonical event (`event_type` + JSON payload) to
    /// per-type and wildcard subscribers (tau `emit_event`).
    pub async fn emit_event(&self, event_type: &str, payload: Value) {
        self.shared.emit_event(event_type, payload).await;
    }

    /// Adapt a core [`AgentEvent`] to Pi's extension-facing session metadata and
    /// dispatch it (tau `_on_agent_event`): `AgentStart` resets the turn index,
    /// `TurnStart` carries `turn_index` + timestamp, `TurnEnd` carries
    /// `turn_index` + message + results and then advances the index.
    pub async fn on_agent_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart(_) => {
                self.turn_index = 0;
                self.shared.emit_event("agent_start", to_json(event)).await;
            }
            AgentEvent::TurnStart(_) => {
                let payload = serde_json::json!({
                    "turn_index": self.turn_index,
                    "timestamp": now_millis(),
                });
                self.shared.emit_event("turn_start", payload).await;
            }
            AgentEvent::TurnEnd(turn) => {
                let payload = serde_json::json!({
                    "turn_index": self.turn_index,
                    "message": turn.message,
                    "tool_results": turn.tool_results,
                });
                self.shared.emit_event("turn_end", payload).await;
                self.turn_index += 1;
            }
            other => {
                self.shared
                    .emit_event(other.event_type(), to_json(other))
                    .await;
            }
        }
    }

    /// Render a custom message via a registered renderer, or `None` for raw
    /// fallback (tau `render_custom_message`).
    pub async fn render_custom_message(
        &self,
        custom_type: &str,
        content: &str,
        details: Option<&Value>,
        expanded: bool,
    ) -> Option<String> {
        self.shared
            .render_custom_message(custom_type, content, details, expanded)
            .await
    }
}

/// Placeholder handler for extension slash commands (see
/// [`ExtensionRuntime::build_command_registry`]).
#[allow(clippy::needless_pass_by_value)] // uniform `fn(CommandContext)` handler signature
fn extension_command_placeholder(context: CommandContext<'_>) -> CommandResult {
    CommandResult {
        handled: true,
        message: Some(format!(
            "Extension command /{} is registered but not executable in this build.",
            context.name
        )),
        ..CommandResult::default()
    }
}

/// Wrap one tool in the `tool_call` → execute → `tool_result` hook seam (tau
/// `_wrap_tool`). Blocking short-circuits with `Tool call blocked: {reason}`; an
/// inner execution error propagates unchanged (result hooks do not run).
fn wrap_tool(shared: Arc<RuntimeShared>, tool: AgentTool) -> AgentTool {
    let inner = tool.execute_fn.clone();
    let name = tool.name.clone();
    let execute: ToolExecutor = Arc::new(move |tool_call_id, arguments, signal, on_update| {
        let shared = shared.clone();
        let inner = inner.clone();
        let name = name.clone();
        Box::pin(async move {
            let call_outcome = shared.run_tool_call_hooks(&name, &arguments).await;
            if call_outcome.block {
                let reason = call_outcome
                    .reason
                    .unwrap_or_else(|| "blocked by an extension".to_string());
                return Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                    TextContent::new(format!("Tool call blocked: {reason}")),
                )]));
            }
            let effective = match call_outcome.arguments {
                Some(value) => value_to_map(value),
                None => arguments,
            };
            let result = inner(tool_call_id, effective.clone(), signal, on_update).await?;
            Ok(shared
                .run_tool_result_hooks(&name, &effective, result)
                .await)
        })
    });
    AgentTool {
        execute_fn: execute,
        ..tool
    }
}

/// Build an [`AgentTool`] whose executor marshals arguments into the guest and
/// unmarshals the result (tau's extension tool, host-brokered).
fn build_extension_tool(host: Arc<dyn ExtensionHost>, extension: &str, def: &ToolDef) -> AgentTool {
    let parameters: JsonMap = def.parameters.as_object().cloned().unwrap_or_default();
    let extension = extension.to_string();
    let tool_name = def.name.clone();
    let execute: ToolExecutor = Arc::new(move |_tool_call_id, arguments, _signal, _on_update| {
        let host = host.clone();
        let extension = extension.clone();
        let tool_name = tool_name.clone();
        Box::pin(async move {
            let value = Value::Object(arguments);
            match host.call_tool(&extension, &tool_name, &value).await {
                Ok(result) => Ok(tool_call_result_to_agent(result)),
                Err(err) => Err(ToolError(host_error_message(&err))),
            }
        })
    });
    let mut tool = AgentTool::new(
        def.name.clone(),
        def.label.clone(),
        def.description.clone(),
        parameters,
        execute,
    );
    tool.prompt_snippet.clone_from(&def.prompt_snippet);
    tool
}

fn tool_call_result_to_agent(result: ToolCallResult) -> AgentToolResult {
    let content = if result.text.is_empty() {
        Vec::new()
    } else {
        vec![ToolResultContent::Text(TextContent::new(result.text))]
    };
    AgentToolResult {
        content,
        details: result.details,
        ..AgentToolResult::default()
    }
}

fn value_to_map(value: Value) -> JsonMap {
    match value {
        Value::Object(map) => map,
        _ => JsonMap::new(),
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn host_error_message(error: &HostError) -> String {
    match error {
        HostError::Dispatch { message, .. } => message.clone(),
        HostError::UnknownExtension(name) => format!("unknown extension: {name}"),
    }
}

fn host_diagnostic_to_resource(diagnostic: &HostDiagnostic) -> ResourceDiagnostic {
    let mut resource = ResourceDiagnostic::new("extension", diagnostic.message.clone())
        .with_severity(if diagnostic.is_error {
            "error"
        } else {
            "warning"
        });
    if !diagnostic.extension.is_empty() {
        resource = resource.with_name(diagnostic.extension.clone());
    }
    if let Some(path) = &diagnostic.path {
        resource = resource.with_path(path.clone());
    }
    resource
}
