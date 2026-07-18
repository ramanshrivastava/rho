//! An in-memory [`ExtensionHost`] for driving the extension-runtime
//! orchestration without a WASM toolchain.
//!
//! Extensions are pre-seeded with their registrations and hook handlers as Rust
//! closures. `load` reports each seeded extension (matched to the discovered
//! specs by name) as a [`LoadedExtension`], and the hook methods invoke the
//! corresponding closures — exactly the surface the wasmtime host will provide,
//! so parity tests exercise the real [`super::ExtensionRuntime`] code paths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use rho_ext_host::{
    AgentHookEvent, CommandDef, ExtensionHost, ExtensionSpec, HostBridge, HostError, InputEvent,
    InputOutcome, LifecycleEvent, LoadOutcome, LoadedExtension, ToolCallEvent, ToolCallOutcome,
    ToolCallResult, ToolDef, ToolResultEvent, ToolResultOutcome,
};

type ToolFn = Arc<dyn Fn(&Value) -> Result<ToolCallResult, HostError> + Send + Sync>;
type ToolCallFn =
    Arc<dyn Fn(&ToolCallEvent) -> Result<Option<ToolCallOutcome>, HostError> + Send + Sync>;
type ToolResultFn =
    Arc<dyn Fn(&ToolResultEvent) -> Result<Option<ToolResultOutcome>, HostError> + Send + Sync>;
type InputFn = Arc<dyn Fn(&InputEvent) -> Result<Option<InputOutcome>, HostError> + Send + Sync>;
type LifecycleFn = Arc<dyn Fn(&LifecycleEvent) -> Result<(), HostError> + Send + Sync>;
type AgentEventFn = Arc<dyn Fn(&AgentHookEvent) -> Result<(), HostError> + Send + Sync>;
type RenderFn = Arc<
    dyn Fn(&str, &str, Option<&Value>, bool) -> Result<Option<String>, HostError> + Send + Sync,
>;

/// A pre-configured extension inside a [`FakeExtensionHost`].
#[derive(Clone, Default)]
pub struct FakeExtension {
    name: String,
    tools: Vec<(ToolDef, ToolFn)>,
    commands: Vec<CommandDef>,
    guidelines: Vec<String>,
    renderers: HashMap<String, RenderFn>,
    tool_call: Option<ToolCallFn>,
    tool_result: Option<ToolResultFn>,
    input: Option<InputFn>,
    session_start: Option<LifecycleFn>,
    session_shutdown: Option<LifecycleFn>,
    agent_specific: HashMap<String, AgentEventFn>,
    agent_wildcard: Option<AgentEventFn>,
}

impl FakeExtension {
    /// Start building an extension named `name`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    /// Register a tool with its executor closure.
    #[must_use]
    pub fn with_tool(
        mut self,
        def: ToolDef,
        run: impl Fn(&Value) -> Result<ToolCallResult, HostError> + Send + Sync + 'static,
    ) -> Self {
        self.tools.push((def, Arc::new(run)));
        self
    }

    /// Register a slash command (metadata only).
    #[must_use]
    pub fn with_command(mut self, command: CommandDef) -> Self {
        self.commands.push(command);
        self
    }

    /// Register a standalone prompt guideline.
    #[must_use]
    pub fn with_guideline(mut self, guideline: &str) -> Self {
        self.guidelines.push(guideline.to_string());
        self
    }

    /// Register a message renderer for `custom_type`.
    #[must_use]
    pub fn with_renderer(
        mut self,
        custom_type: &str,
        render: impl Fn(&str, &str, Option<&Value>, bool) -> Result<Option<String>, HostError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.renderers
            .insert(custom_type.to_string(), Arc::new(render));
        self
    }

    /// Subscribe a `tool_call` hook.
    #[must_use]
    pub fn on_tool_call(
        mut self,
        hook: impl Fn(&ToolCallEvent) -> Result<Option<ToolCallOutcome>, HostError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.tool_call = Some(Arc::new(hook));
        self
    }

    /// Subscribe a `tool_result` hook.
    #[must_use]
    pub fn on_tool_result(
        mut self,
        hook: impl Fn(&ToolResultEvent) -> Result<Option<ToolResultOutcome>, HostError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.tool_result = Some(Arc::new(hook));
        self
    }

    /// Subscribe an `input` hook.
    #[must_use]
    pub fn on_input(
        mut self,
        hook: impl Fn(&InputEvent) -> Result<Option<InputOutcome>, HostError> + Send + Sync + 'static,
    ) -> Self {
        self.input = Some(Arc::new(hook));
        self
    }

    /// Subscribe a `session_start` handler.
    #[must_use]
    pub fn on_session_start(
        mut self,
        hook: impl Fn(&LifecycleEvent) -> Result<(), HostError> + Send + Sync + 'static,
    ) -> Self {
        self.session_start = Some(Arc::new(hook));
        self
    }

    /// Subscribe a `session_shutdown` handler.
    #[must_use]
    pub fn on_session_shutdown(
        mut self,
        hook: impl Fn(&LifecycleEvent) -> Result<(), HostError> + Send + Sync + 'static,
    ) -> Self {
        self.session_shutdown = Some(Arc::new(hook));
        self
    }

    /// Subscribe a handler for a specific agent-event type.
    #[must_use]
    pub fn on_agent_event(
        mut self,
        event_type: &str,
        hook: impl Fn(&AgentHookEvent) -> Result<(), HostError> + Send + Sync + 'static,
    ) -> Self {
        self.agent_specific
            .insert(event_type.to_string(), Arc::new(hook));
        self
    }

    /// Subscribe the wildcard (`agent_event`) handler that sees every event.
    #[must_use]
    pub fn on_any_agent_event(
        mut self,
        hook: impl Fn(&AgentHookEvent) -> Result<(), HostError> + Send + Sync + 'static,
    ) -> Self {
        self.agent_wildcard = Some(Arc::new(hook));
        self
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subscriptions = Vec::new();
        if self.tool_call.is_some() {
            subscriptions.push("tool_call".to_string());
        }
        if self.tool_result.is_some() {
            subscriptions.push("tool_result".to_string());
        }
        if self.input.is_some() {
            subscriptions.push("input".to_string());
        }
        if self.session_start.is_some() {
            subscriptions.push("session_start".to_string());
        }
        if self.session_shutdown.is_some() {
            subscriptions.push("session_shutdown".to_string());
        }
        for event_type in self.agent_specific.keys() {
            subscriptions.push(event_type.clone());
        }
        if self.agent_wildcard.is_some() {
            subscriptions.push("agent_event".to_string());
        }
        subscriptions
    }

    fn loaded(&self) -> LoadedExtension {
        LoadedExtension {
            name: self.name.clone(),
            tools: self.tools.iter().map(|(def, _)| def.clone()).collect(),
            commands: self.commands.clone(),
            guidelines: self.guidelines.clone(),
            message_renderers: self.renderers.keys().cloned().collect(),
            subscriptions: self.subscriptions(),
            registers_key_interceptor: false,
        }
    }
}

/// An in-memory [`ExtensionHost`] backed by [`FakeExtension`] closures.
#[derive(Default)]
pub struct FakeExtensionHost {
    extensions: Mutex<HashMap<String, FakeExtension>>,
}

impl FakeExtensionHost {
    /// An empty host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed (or replace) an extension definition.
    pub fn insert(&self, extension: FakeExtension) {
        self.extensions
            .lock()
            .expect("extensions lock")
            .insert(extension.name.clone(), extension);
    }

    /// A host pre-seeded with `extensions`, wrapped in an `Arc` for the runtime.
    #[must_use]
    pub fn with(extensions: impl IntoIterator<Item = FakeExtension>) -> Arc<Self> {
        let host = Self::new();
        for extension in extensions {
            host.insert(extension);
        }
        Arc::new(host)
    }

    fn get(&self, name: &str) -> Result<FakeExtension, HostError> {
        self.extensions
            .lock()
            .expect("extensions lock")
            .get(name)
            .cloned()
            .ok_or_else(|| HostError::UnknownExtension(name.to_string()))
    }
}

#[async_trait]
impl ExtensionHost for FakeExtensionHost {
    async fn load(&self, specs: &[ExtensionSpec], _bridge: Arc<dyn HostBridge>) -> LoadOutcome {
        let registered = self.extensions.lock().expect("extensions lock");
        let mut extensions = Vec::new();
        let mut diagnostics = Vec::new();
        for spec in specs {
            match registered.get(&spec.name) {
                Some(extension) => extensions.push(extension.loaded()),
                None => diagnostics.push(HostDiagnosticShim::unknown(&spec.name)),
            }
        }
        LoadOutcome {
            extensions,
            diagnostics,
        }
    }

    async fn bind(&self, _bridge: Arc<dyn HostBridge>) {}

    async fn call_tool(
        &self,
        extension: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<ToolCallResult, HostError> {
        let ext = self.get(extension)?;
        let run = ext
            .tools
            .iter()
            .find(|(def, _)| def.name == tool)
            .map(|(_, run)| run.clone())
            .ok_or_else(|| {
                HostError::dispatch(extension, "call_tool", format!("unknown tool: {tool}"))
            })?;
        run(arguments)
    }

    async fn on_input(
        &self,
        extension: &str,
        event: &InputEvent,
    ) -> Result<Option<InputOutcome>, HostError> {
        match self.get(extension)?.input {
            Some(hook) => hook(event),
            None => Ok(None),
        }
    }

    async fn on_tool_call(
        &self,
        extension: &str,
        event: &ToolCallEvent,
    ) -> Result<Option<ToolCallOutcome>, HostError> {
        match self.get(extension)?.tool_call {
            Some(hook) => hook(event),
            None => Ok(None),
        }
    }

    async fn on_tool_result(
        &self,
        extension: &str,
        event: &ToolResultEvent,
    ) -> Result<Option<ToolResultOutcome>, HostError> {
        match self.get(extension)?.tool_result {
            Some(hook) => hook(event),
            None => Ok(None),
        }
    }

    async fn on_session_start(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        match self.get(extension)?.session_start {
            Some(hook) => hook(event),
            None => Ok(()),
        }
    }

    async fn on_session_shutdown(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        match self.get(extension)?.session_shutdown {
            Some(hook) => hook(event),
            None => Ok(()),
        }
    }

    async fn on_agent_event(
        &self,
        extension: &str,
        event: &AgentHookEvent,
    ) -> Result<(), HostError> {
        let ext = self.get(extension)?;
        // The guest routes one dispatch to both its specific-type handler and
        // its wildcard handler (mirrors tau's separate handler lists).
        if let Some(hook) = ext.agent_specific.get(&event.event_type) {
            hook(event)?;
        }
        if let Some(hook) = &ext.agent_wildcard {
            hook(event)?;
        }
        Ok(())
    }

    async fn render_message(
        &self,
        extension: &str,
        custom_type: &str,
        content: &str,
        details: Option<&Value>,
        expanded: bool,
    ) -> Result<Option<String>, HostError> {
        match self.get(extension)?.renderers.get(custom_type) {
            Some(render) => render(custom_type, content, details, expanded),
            None => Ok(None),
        }
    }

    async fn teardown(&self) {}
}

/// Small shim so this module needn't import the diagnostic constructors.
struct HostDiagnosticShim;

impl HostDiagnosticShim {
    fn unknown(name: &str) -> rho_ext_host::HostDiagnostic {
        rho_ext_host::HostDiagnostic::warning(name.to_string(), "extension not seeded in fake host")
    }
}
