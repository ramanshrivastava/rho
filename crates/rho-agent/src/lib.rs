//! `rho-agent` — the portable, provider-neutral agent core (Rust port of tau's
//! `tau_agent` package).
//!
//! This is the base crate of the rho workspace. It owns the pieces that do not
//! depend on any specific model vendor or user interface:
//!
//! - [`messages`] — Pi-compatible content blocks and transcript message models
//!   with **byte-identical** JSON serialization to tau's `WireModel` types.
//! - [`events`] — the agent-level event stream ([`events::AgentEvent`]).
//! - [`provider_events`] — the provider streaming events
//!   ([`provider_events::AssistantMessageEvent`]).
//! - [`tools`] — the provider-neutral tool-result wire type.
//! - [`session`] — append-only session entries plus JSONL (de)serialization with
//!   Tau-v1 legacy migration.
//! - [`types`] — the shared free-form JSON value alias.
//!
//! ## Byte-compatibility is the contract
//!
//! Every type here round-trips the golden fixtures in `fixtures/` **byte for
//! byte**. The serde idioms that make this work — untagged unions with
//! `monostate` discriminators (so a non-leading `type` field keeps its position),
//! `camelCase` message keys vs `snake_case` entry keys, per-field
//! `skip_serializing_if` to reproduce `exclude_none`, and the `cacheWrite1H`
//! rename trap — are documented at each definition and in `dev-notes/phase-1.md`.
//!
//! Layering rule: this crate must never depend on `rho-ai` or `rho-coding`.
//! Cargo's acyclic dependency graph enforces what tau could only document,
//! dissolving the historical `tau_agent` ↔ `tau_ai` import cycle.

#[path = "loop.rs"]
pub mod agent_loop;
pub mod clock;
pub mod events;
#[cfg(feature = "fake")]
pub mod fake;
pub mod harness;
pub mod messages;
pub mod model_limits;
pub mod provider;
pub mod provider_events;
pub mod session;
pub mod tools;
pub mod types;
