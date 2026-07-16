//! `rho-coding` — the coding-agent application layer (port of tau's `tau_coding`).
//!
//! This crate composes [`rho_agent`] and [`rho_ai`] into a real coding assistant:
//! filesystem and shell tools, the stateful `CodingSession`, slash commands,
//! skills, the provider catalog, OAuth flows, session export, and the event
//! renderers used by print mode.
//!
//! Planned contents:
//!
//! - **tools** — read/write/edit/bash and friends, plus the tool catalog.
//! - **session** — `CodingSession` and its `SessionOwnEvent` stream (compaction,
//!   steering queue updates, auto-retry, …) layered over the agent harness.
//! - **commands / skills / catalog** — slash-command registry, skill invocation,
//!   and config-driven provider catalog loading.
//! - **oauth** — device-flow and provider OAuth (Anthropic, GitHub Copilot, …).
//! - **export / rendering** — HTML session export and the JSON/plain/transcript
//!   event renderers (`tau -p` parity).
//!
//! Layering: depends on [`rho_ai`] and [`rho_agent`]; consumed by `rho-tui` and
//! the `rho` binary.
//!
//! Milestone M0 ships this crate as an empty scaffold; the vertical slice lands
//! in M4a and the full surface in M4b.
