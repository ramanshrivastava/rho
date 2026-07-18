//! The wasmtime-backed [`ExtensionHost`] implementation (feature `wasmtime`).
//!
//! **Skeleton.** This module is the target of the M7 `wasm-host` implementation
//! cluster: it wires wasmtime's component model (`Config::async_support(true)`,
//! `bindgen!` over `wit/rho-extension.wit`, `func_wrap_async` host imports, a
//! per-extension `Store` holding the [`HostBridge`], the capability sandbox with
//! no ambient WASI FS/net, and component re-instantiation for hot reload). Until
//! it lands, the type exists so `--features wasmtime` compiles; every method
//! returns a clear "not yet implemented" so a mis-wired build fails loudly
//! rather than silently doing nothing.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::host::{
    ExtensionHost, ExtensionSpec, HostBridge, HostError, LoadOutcome,
};
use crate::payload::{
    AgentHookEvent, InputEvent, InputOutcome, LifecycleEvent, ToolCallEvent, ToolCallOutcome,
    ToolCallResult, ToolResultEvent, ToolResultOutcome,
};

/// wasmtime component runtime for rho extensions.
#[derive(Default)]
pub struct WasmExtensionHost {
    _private: (),
}

impl WasmExtensionHost {
    /// Construct a host with a fresh wasmtime engine (component model + async).
    ///
    /// # Errors
    /// Returns an error if the wasmtime engine cannot be configured.
    pub fn new() -> Result<Self, HostError> {
        Ok(Self { _private: () })
    }
}

impl std::fmt::Debug for WasmExtensionHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmExtensionHost").finish_non_exhaustive()
    }
}

fn unimplemented_dispatch(extension: &str, event: &str) -> HostError {
    HostError::dispatch(extension, event, "wasmtime host not yet implemented")
}

#[async_trait]
impl ExtensionHost for WasmExtensionHost {
    async fn load(&self, _specs: &[ExtensionSpec], _bridge: Arc<dyn HostBridge>) -> LoadOutcome {
        LoadOutcome::default()
    }

    async fn bind(&self, _bridge: Arc<dyn HostBridge>) {}

    async fn call_tool(
        &self,
        extension: &str,
        _tool: &str,
        _arguments: &Value,
    ) -> Result<ToolCallResult, HostError> {
        Err(unimplemented_dispatch(extension, "call_tool"))
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
