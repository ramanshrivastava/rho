//! `rho-ai` — the provider and model-streaming layer (port of tau's `tau_ai`).
//!
//! This crate turns the transcript and tool definitions from [`rho_agent`] into
//! concrete HTTP requests against model vendors, and canonicalizes each vendor's
//! streaming wire format back into the provider-neutral `AssistantMessageEvent`
//! sequence the agent loop consumes.
//!
//! Planned contents:
//!
//! - **six provider adapters** — `anthropic`, `openai_compatible`, `openai_codex`,
//!   `google`, `mistral`, and the deterministic `fake` provider used by tests and
//!   golden fixtures.
//! - **http / sse** — the streaming HTTP client and Server-Sent-Events decoder.
//! - **retry** — transient-status retry with backoff and cancellation.
//! - **canonicalization** — per-vendor delta accumulation into canonical events.
//!
//! Layering: depends only on [`rho_agent`]; it must not depend on `rho-coding`.
//!
//! Milestone M0 ships this crate as an empty scaffold; adapters land in M3.
