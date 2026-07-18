//! The wasmtime-backed [`ExtensionHost`] implementation (feature `wasmtime`).
//!
//! A real wasmtime **component-model** runtime for rho extensions, built on the
//! locked plan in `dev-notes/m7-extension-design.md`:
//!
//! * `Config::async_support(true)` + the component model; host imports are wired
//!   with the async `bindgen!` bindings so the guest can await host work
//!   (`func_wrap_async` under the hood).
//! * One [`wasmtime::Store`] + component instance per extension. `load`
//!   instantiates each component, runs `init` (collecting registrations and hook
//!   subscriptions), and keeps the instances live for later hook dispatch.
//! * **Capability sandbox.** WASI is linked so a `wasm32-wasip2` guest can
//!   instantiate, but with an empty [`WasiCtx`] — no preopens, no inherited
//!   stdio/env, no sockets. A guest that reaches for the filesystem or network
//!   fails cleanly (the import exists but denies) — the sandbox-denial
//!   guarantee.
//! * **Init-phase-only registration.** The store tracks an `in_init` flag;
//!   `register-*`/`subscribe` host imports are honored only while `init` runs,
//!   mirroring tau's "registration only during setup".
//! * **Hot reload.** `load` clears and re-instantiates the whole set from disk,
//!   matching tau's `/reload` generation invalidation.
//!
//! Free-form JSON crosses the boundary as canonical JSON **text** (the WIT ABI
//! decision); this module serializes [`serde_json::Value`] on the way in and
//! parses on the way out.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

/// CPU bound per guest call: a hook or tool invocation that burns this much
/// fuel (a rough proxy for executed Wasm instructions) traps instead of hanging
/// the host. Generous enough for legitimate work; a `loop {}` guest trips it.
const FUEL_PER_CALL: u64 = 2_000_000_000;

/// How often a running guest yields back to the async executor (every this many
/// fuel units). This makes a compute-bound loop cooperatively yield so the
/// wall-clock [`DISPATCH_TIMEOUT`] can preempt it even before fuel is exhausted.
const FUEL_YIELD_INTERVAL: u64 = 10_000_000;

/// Wall-clock ceiling for a single guest dispatch. A guest that blocks (a slow
/// host import, a huge-but-finite loop) is cancelled at this bound; combined
/// with fuel yielding, no single call can wedge a dispatch indefinitely.
const DISPATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Memory ceiling per extension store: a guest that tries to grow past this
/// fails the allocation cleanly rather than exhausting host memory.
const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host::{
    ExtensionHost, ExtensionSpec, HostBridge, HostDiagnostic, HostError, LoadOutcome,
    LoadedExtension,
};
use crate::payload::{
    AgentHookEvent, CommandDef, InputAction, InputEvent, InputOutcome, LifecycleEvent,
    ToolCallEvent, ToolCallOutcome, ToolCallResult, ToolDef, ToolResultEvent, ToolResultOutcome,
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../rho-ext-api/wit/rho-extension.wit",
        world: "extension",
        imports: { default: async },
        exports: { default: async },
    });
}

use bindings::rho::extension::types as wit;

/// Registrations and subscriptions collected while a guest's `init` runs.
#[derive(Default)]
struct InitCollector {
    tools: Vec<ToolDef>,
    commands: Vec<CommandDef>,
    guidelines: Vec<String>,
    message_renderers: Vec<String>,
    subscriptions: Vec<String>,
}

/// Per-store state: the injected host bridge, the capability-sandbox WASI ctx,
/// and (during `init`) the registration collector.
struct StoreState {
    bridge: Arc<dyn HostBridge>,
    in_init: bool,
    collector: InitCollector,
    wasi: WasiCtx,
    table: ResourceTable,
    /// Memory/table/instance ceilings enforced by wasmtime (the resource
    /// sandbox, complementing the empty-`WasiCtx` capability sandbox).
    limits: StoreLimits,
}

impl StoreState {
    fn new(bridge: Arc<dyn HostBridge>) -> Self {
        Self {
            bridge,
            in_init: false,
            collector: InitCollector::default(),
            // The sandbox: an empty context. No preopens, no inherited stdio,
            // no env, no sockets. A guest FS/net attempt therefore denies.
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY_BYTES)
                .build(),
        }
    }
}

impl WasiView for StoreState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A loaded, live extension: its component instance bindings and the store that
/// backs them.
struct Instance {
    store: Store<StoreState>,
    bindings: bindings::Extension,
}

struct Inner {
    /// Per-extension instances behind their own locks. The subsystem lock
    /// (`WasmExtensionHost::inner`) is held only long enough to *clone* the
    /// `Arc`; the guest then runs while holding only its own instance lock, so
    /// a looping guest can never wedge `load`/`teardown`/other extensions.
    instances: HashMap<String, Arc<Mutex<Instance>>>,
}

/// A dispatch that overran [`DISPATCH_TIMEOUT`].
fn timed_out(extension: &str, event: &str) -> HostError {
    HostError::dispatch(
        extension,
        event,
        format!("timed out after {DISPATCH_TIMEOUT:?}"),
    )
}

/// wasmtime component runtime for rho extensions.
pub struct WasmExtensionHost {
    engine: Engine,
    linker: Linker<StoreState>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for WasmExtensionHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmExtensionHost").finish_non_exhaustive()
    }
}

impl WasmExtensionHost {
    /// Construct a host with a fresh wasmtime engine (component model + async)
    /// and a linker carrying the sandboxed WASI imports and the `rho:extension`
    /// host imports.
    ///
    /// # Errors
    /// Returns an error if the engine cannot be configured or the host imports
    /// cannot be added to the linker.
    pub fn new() -> Result<Self, HostError> {
        let mut config = Config::new();
        // Async is always enabled with the `async` feature in wasmtime 46
        // (`async_support` is a deprecated no-op); the component model is on.
        config.wasm_component_model(true);
        // Meter execution so a runaway guest (e.g. `loop {}` in a hook) traps
        // instead of hanging the host; fuel is reset before every guest call.
        config.consume_fuel(true);
        let engine =
            Engine::new(&config).map_err(|e| HostError::dispatch("", "engine", e.to_string()))?;

        let mut linker: Linker<StoreState> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|e| HostError::dispatch("", "wasi-linker", e.to_string()))?;
        bindings::Extension::add_to_linker::<_, HasSelf<StoreState>>(&mut linker, |s| s)
            .map_err(|e| HostError::dispatch("", "host-linker", e.to_string()))?;

        Ok(Self {
            engine,
            linker,
            inner: Mutex::new(Inner {
                instances: HashMap::new(),
            }),
        })
    }

    /// Clone the lock guarding one loaded extension, holding the subsystem lock
    /// only for the clone. The caller then locks the returned handle and runs
    /// the guest without blocking the rest of the subsystem.
    async fn acquire(&self, extension: &str) -> Result<Arc<Mutex<Instance>>, HostError> {
        let inner = self.inner.lock().await;
        inner
            .instances
            .get(extension)
            .cloned()
            .ok_or_else(|| HostError::UnknownExtension(extension.to_string()))
    }

    /// Instantiate one component, run its `init`, and return the registrations
    /// it produced alongside the live instance.
    async fn instantiate(
        &self,
        spec: &ExtensionSpec,
        bridge: Arc<dyn HostBridge>,
    ) -> Result<(LoadedExtension, Instance), String> {
        let component = Component::from_file(&self.engine, &spec.path)
            .map_err(|e| format!("failed to load component: {e}"))?;

        let mut state = StoreState::new(bridge);
        state.in_init = true;
        let mut store = Store::new(&self.engine, state);
        // Enforce the memory ceiling, give `init` its fuel budget, and make the
        // guest yield periodically so a wall-clock timeout can preempt it.
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| format!("fuel init failed: {e}"))?;
        store.fuel_async_yield_interval(Some(FUEL_YIELD_INTERVAL));

        let instance = bindings::Extension::instantiate_async(&mut store, &component, &self.linker)
            .await
            .map_err(|e| format!("instantiation failed: {e}"))?;

        instance
            .call_init(&mut store)
            .await
            .map_err(|e| format!("init trapped: {e}"))?;

        // Registration is honored only during `init`; close the window and take
        // what the guest registered.
        let data = store.data_mut();
        data.in_init = false;
        let collector = std::mem::take(&mut data.collector);

        let loaded = LoadedExtension {
            name: spec.name.clone(),
            tools: collector.tools,
            commands: collector.commands,
            guidelines: collector.guidelines,
            message_renderers: collector.message_renderers,
            subscriptions: collector.subscriptions,
        };

        Ok((
            loaded,
            Instance {
                store,
                bindings: instance,
            },
        ))
    }
}

/// Parse a JSON string, defaulting to `null` on any error (a guest sending
/// malformed JSON should not crash the host).
fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn to_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

impl From<wit::ToolDef> for ToolDef {
    fn from(t: wit::ToolDef) -> Self {
        ToolDef {
            name: t.name,
            label: t.label,
            description: t.description,
            parameters: parse_json(&t.parameters_json),
            prompt_snippet: t.prompt_snippet,
        }
    }
}

impl From<wit::CommandDef> for CommandDef {
    fn from(c: wit::CommandDef) -> Self {
        CommandDef {
            name: c.name,
            description: c.description,
            usage: c.usage,
            aliases: c.aliases,
        }
    }
}

// -- host imports: serviced from the injected `HostBridge` -------------------

// The `types` interface carries only records; its generated `Host` trait is a
// marker with no methods.
impl bindings::rho::extension::types::Host for StoreState {}

impl bindings::rho::extension::host::Host for StoreState {
    async fn register_tool(&mut self, tool: wit::ToolDef) {
        if !self.in_init {
            return;
        }
        let def: ToolDef = tool.into();
        if self.collector.tools.iter().any(|t| t.name == def.name) {
            return; // first registration per name wins (tau parity)
        }
        self.collector.tools.push(def);
    }

    async fn register_command(&mut self, cmd: wit::CommandDef) {
        if !self.in_init {
            return;
        }
        let def: CommandDef = cmd.into();
        if self.collector.commands.iter().any(|c| c.name == def.name) {
            return;
        }
        self.collector.commands.push(def);
    }

    async fn add_prompt_guideline(&mut self, guideline: String) {
        if !self.in_init {
            return;
        }
        self.collector.guidelines.push(guideline);
    }

    async fn register_message_renderer(&mut self, custom_type: String) {
        if !self.in_init {
            return;
        }
        if self.collector.message_renderers.contains(&custom_type) {
            return;
        }
        self.collector.message_renderers.push(custom_type);
    }

    async fn subscribe(&mut self, event: String) {
        if !self.in_init {
            return;
        }
        if self.collector.subscriptions.contains(&event) {
            return;
        }
        self.collector.subscriptions.push(event);
    }

    async fn get_cwd(&mut self) -> String {
        self.bridge.cwd().await
    }

    async fn get_model(&mut self) -> String {
        self.bridge.model().await
    }

    async fn get_provider_name(&mut self) -> String {
        self.bridge.provider_name().await
    }

    async fn get_session_id(&mut self) -> Option<String> {
        self.bridge.session_id().await
    }

    async fn get_system_prompt(&mut self) -> String {
        self.bridge.system_prompt().await
    }

    async fn is_running(&mut self) -> bool {
        self.bridge.is_running().await
    }

    async fn get_transcript_json(&mut self) -> String {
        self.bridge.transcript_json().await
    }

    async fn notify(&mut self, message: String, level: String) {
        self.bridge.notify(&message, &level).await;
    }

    async fn ui_select(&mut self, title: String, options: Vec<String>) -> Option<String> {
        self.bridge.ui_select(&title, &options).await
    }

    async fn ui_confirm(&mut self, title: String, message: String) -> bool {
        self.bridge.ui_confirm(&title, &message).await
    }

    async fn ui_input(&mut self, title: String, placeholder: String) -> Option<String> {
        self.bridge.ui_input(&title, &placeholder).await
    }

    async fn send_user_message(&mut self, content: String, deliver_as: String) {
        self.bridge.send_user_message(&content, &deliver_as).await;
    }
}

/// Map a wasmtime dispatch error into a [`HostError::Dispatch`].
fn dispatch_err<'a>(
    extension: &'a str,
    event: &'a str,
) -> impl Fn(wasmtime::Error) -> HostError + 'a {
    move |e| HostError::dispatch(extension, event, e.to_string())
}

#[async_trait]
impl ExtensionHost for WasmExtensionHost {
    async fn load(&self, specs: &[ExtensionSpec], bridge: Arc<dyn HostBridge>) -> LoadOutcome {
        let mut inner = self.inner.lock().await;
        // Hot reload: replace the whole instance set (re-instantiate from disk).
        inner.instances.clear();

        let mut outcome = LoadOutcome::default();
        for spec in specs {
            match self.instantiate(spec, bridge.clone()).await {
                Ok((loaded, instance)) => {
                    inner
                        .instances
                        .insert(spec.name.clone(), Arc::new(Mutex::new(instance)));
                    outcome.extensions.push(loaded);
                }
                Err(message) => {
                    outcome
                        .diagnostics
                        .push(HostDiagnostic::error(spec.name.clone(), message));
                }
            }
        }
        outcome
    }

    async fn bind(&self, bridge: Arc<dyn HostBridge>) {
        // Clone the Arcs under a brief subsystem lock, then update each instance
        // under its own lock — never holding both at once.
        let arcs: Vec<Arc<Mutex<Instance>>> = {
            let inner = self.inner.lock().await;
            inner.instances.values().cloned().collect()
        };
        for arc in arcs {
            arc.lock().await.store.data_mut().bridge = bridge.clone();
        }
    }

    async fn call_tool(
        &self,
        extension: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<ToolCallResult, HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);

        let args_json = to_json(arguments);
        let call = inst
            .bindings
            .call_call_tool(&mut inst.store, tool, &args_json);
        let result_json = match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "call_tool"))?,
            Err(_) => return Err(timed_out(extension, "call_tool")),
        };

        let value: Value = serde_json::from_str(&result_json).map_err(|e| {
            HostError::dispatch(extension, "call_tool", format!("bad tool result JSON: {e}"))
        })?;
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Distinguish an absent `details` (→ `None`, omitted downstream) from a
        // present literal `null` (→ `Some(Value::Null)`, preserved) — a byte-compat
        // requirement for free-form payloads. `map.get` is key-presence aware where
        // a plain `Option<Value>` deserialize collapses both to `None`.
        let details = match &value {
            Value::Object(map) => map.get("details").cloned(),
            _ => None,
        };
        Ok(ToolCallResult { text, details })
    }

    async fn on_input(
        &self,
        extension: &str,
        event: &InputEvent,
    ) -> Result<Option<InputOutcome>, HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);

        let ev = wit::InputEvent {
            text: event.text.clone(),
            source: event.source.clone(),
            streaming_behavior: event.streaming_behavior.clone(),
        };
        let call = inst.bindings.call_on_input(&mut inst.store, &ev);
        let out = match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_input"))?,
            Err(_) => return Err(timed_out(extension, "on_input")),
        };

        Ok(out.map(|o| {
            let action = match o.action.as_str() {
                "transform" => InputAction::Transform,
                "handled" => InputAction::Handled,
                _ => InputAction::Continue,
            };
            InputOutcome {
                action,
                text: o.text,
                message: o.message,
            }
        }))
    }

    async fn on_tool_call(
        &self,
        extension: &str,
        event: &ToolCallEvent,
    ) -> Result<Option<ToolCallOutcome>, HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);

        let ev = wit::ToolCallEvent {
            tool_name: event.tool_name.clone(),
            arguments_json: to_json(&event.arguments),
        };
        let call = inst.bindings.call_on_tool_call(&mut inst.store, &ev);
        let out = match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_tool_call"))?,
            Err(_) => return Err(timed_out(extension, "on_tool_call")),
        };

        Ok(out.map(|o| ToolCallOutcome {
            block: o.block,
            reason: o.reason,
            arguments: o.arguments_json.as_deref().map(parse_json),
        }))
    }

    async fn on_tool_result(
        &self,
        extension: &str,
        event: &ToolResultEvent,
    ) -> Result<Option<ToolResultOutcome>, HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);

        let ev = wit::ToolResultEvent {
            tool_name: event.tool_name.clone(),
            arguments_json: to_json(&event.arguments),
            result_text: event.result_text.clone(),
            result_details_json: event.result_details.as_ref().map(to_json),
        };
        let call = inst.bindings.call_on_tool_result(&mut inst.store, &ev);
        let out = match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_tool_result"))?,
            Err(_) => return Err(timed_out(extension, "on_tool_result")),
        };

        Ok(out.map(|o| ToolResultOutcome {
            content: o.content,
            details: o.details_json.as_deref().map(parse_json),
        }))
    }

    async fn on_session_start(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);
        let ev = wit::LifecycleEvent {
            reason: event.reason.clone(),
        };
        let call = inst.bindings.call_on_session_start(&mut inst.store, &ev);
        match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_session_start")),
            Err(_) => Err(timed_out(extension, "on_session_start")),
        }
    }

    async fn on_session_shutdown(
        &self,
        extension: &str,
        event: &LifecycleEvent,
    ) -> Result<(), HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);
        let ev = wit::LifecycleEvent {
            reason: event.reason.clone(),
        };
        let call = inst.bindings.call_on_session_shutdown(&mut inst.store, &ev);
        match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_session_shutdown")),
            Err(_) => Err(timed_out(extension, "on_session_shutdown")),
        }
    }

    async fn on_agent_event(
        &self,
        extension: &str,
        event: &AgentHookEvent,
    ) -> Result<(), HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);
        let payload_json = to_json(&event.payload);
        let call =
            inst.bindings
                .call_on_agent_event(&mut inst.store, &event.event_type, &payload_json);
        match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "on_agent_event")),
            Err(_) => Err(timed_out(extension, "on_agent_event")),
        }
    }

    async fn render_message(
        &self,
        extension: &str,
        custom_type: &str,
        content: &str,
        details: Option<&Value>,
        expanded: bool,
    ) -> Result<Option<String>, HostError> {
        let arc = self.acquire(extension).await?;
        let mut guard = arc.lock().await;
        let inst = &mut *guard;
        let _ = inst.store.set_fuel(FUEL_PER_CALL);
        let details_json = details.map(to_json);
        let call = inst.bindings.call_render_message(
            &mut inst.store,
            custom_type,
            content,
            details_json.as_deref(),
            expanded,
        );
        match tokio::time::timeout(DISPATCH_TIMEOUT, call).await {
            Ok(r) => r.map_err(dispatch_err(extension, "render_message")),
            Err(_) => Err(timed_out(extension, "render_message")),
        }
    }

    async fn teardown(&self) {
        let mut inner = self.inner.lock().await;
        inner.instances.clear();
    }
}
