//! `rho-ext-host` — the extension host abstraction and its wasmtime backing.
//!
//! This crate is the Rust analogue of tau's `tau_coding.extensions` runtime, but
//! split along the seam the codex-rs study argued for (see
//! `dev-notes/m7-extension-design.md`): the tau-parity *orchestration*
//! (hook chaining, first-wins registration, generation staleness, diagnostics)
//! lives in `rho-coding`, on top of the [`ExtensionHost`] trait defined here.
//! The trait has two implementations:
//!
//! * [`NoopExtensionHost`] — always available, zero WASM machinery. Default rho
//!   builds link only this, so `cargo build` never compiles wasmtime.
//! * [`wasm::WasmExtensionHost`] — the real component runtime, behind the
//!   `wasmtime` feature (off by default). Guests are compiled `wasm32-wasip2`
//!   components implementing the `rho:extension` WIT world (`wit/`).
//!
//! Because `rho-coding` depends only on the trait, the wasmtime dependency stays
//! optional and a future process/MCP transport could slot in beside WASM without
//! touching the session — the transport-neutral seam from the design note.

pub mod discovery;
pub mod host;
pub mod payload;

#[cfg(feature = "wasmtime")]
pub mod wasm;

pub use discovery::{DiscoveryPaths, discover_extensions, extension_dirs};
pub use host::{
    ExtensionHost, ExtensionSpec, HostBridge, HostDiagnostic, HostError, LoadOutcome,
    LoadedExtension, NoopExtensionHost,
};
pub use payload::{
    AgentHookEvent, CommandDef, InputAction, InputEvent, InputOutcome, LifecycleEvent,
    ToolCallEvent, ToolCallOutcome, ToolCallResult, ToolDef, ToolResultEvent, ToolResultOutcome,
    TurnEndEvent, TurnStartEvent,
};
