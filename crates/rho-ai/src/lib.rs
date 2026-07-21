//! `rho-ai` — provider adapters and the model streaming layer (Rust port of
//! tau's `tau_ai` package).
//!
//! This crate turns provider HTTP/SSE into the canonical
//! [`rho_agent::provider_events::AssistantMessageEvent`] stream every
//! [`rho_agent::provider::ModelProvider`] speaks. It owns:
//!
//! - [`stream`] — the canonical-event accumulator (tau's
//!   `canonicalize_provider_stream`), driven **directly** by adapters. tau's
//!   transitional `_provider_events` layer is intentionally not ported; see the
//!   module docs and `dev-notes/phase-3.md`.
//! - [`engine`] — the shared HTTP + retry + SSE line-splitting envelope.
//! - [`http`] / [`retry`] / [`http_errors`] / [`env`] — the reqwest client,
//!   retry policy, safe error extraction, and config structs.
//! - The six adapters: [`anthropic`], [`openai_compatible`], [`openai_codex`],
//!   [`google`], [`mistral`], plus the re-exported [`FakeProvider`].
//!
//! Layering: `rho-ai` depends only on `rho-agent` (+ external crates). The
//! canonical `provider`/`api` labels and byte-exact request payloads are pinned
//! against the `fixtures/sse/` oracle by the golden tests.

pub mod anthropic;
pub mod engine;
pub mod env;
pub mod google;
pub mod http;
pub mod http_errors;
pub mod mistral;
pub mod model_limits;
pub mod openai_codex;
pub mod openai_compatible;
pub mod retry;
pub mod stream;
pub mod types;
pub mod util;
pub mod wire;

pub use anthropic::AnthropicProvider;
pub use env::{
    AnthropicConfig, OpenAICodexConfig, OpenAICodexCredentials, OpenAICompatibleConfig,
    RuntimeProviderAuth, openai_compatible_config_from_env,
};
pub use google::GoogleGenerativeAIProvider;
pub use mistral::MistralConversationsProvider;
pub use model_limits::RuntimeModelLimits;
pub use openai_codex::OpenAICodexProvider;
pub use openai_compatible::OpenAICompatibleProvider;

/// The scriptable test double (tau places `FakeProvider` in `tau_ai`; rho keeps
/// it in `rho-agent` so the agent-core tests can drive the provider seam without
/// a `rho-agent -> rho-ai` dependency — see `rho_agent::fake`). It is re-exported
/// here so `rho_ai::FakeProvider` matches tau's `tau_ai.FakeProvider` import
/// path.
pub use rho_agent::fake::FakeProvider;

/// Re-exported so adapters and callers share one provider contract.
pub use rho_agent::provider::{CancellationToken, ModelProvider};
