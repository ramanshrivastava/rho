//! `rho-agent` — the portable, provider-neutral agent core.
//!
//! This is the base crate of the rho workspace and the Rust port of tau's
//! `tau_agent` Python package. It will own the pieces that do not depend on any
//! specific model vendor or user interface:
//!
//! - **messages** — Pi-compatible content blocks and transcript message models
//!   (`UserMessage`, `AssistantMessage`, `ToolResultMessage`, …) with
//!   byte-identical JSON serialization to tau's `WireModel` types.
//! - **events** — the agent-level event stream (`AgentEvent`) and the provider
//!   streaming events (`AssistantMessageEvent`).
//! - **provider** — the [`ModelProvider`]-equivalent trait the agent loop drives.
//! - **tools** — provider-neutral tool definitions and results.
//! - **loop / harness** — the pure agent loop and the stateful harness built on
//!   top of it.
//! - **session** — append-only session entries, JSONL (de)serialization with
//!   Tau-v1 migration, the session tree, and derived [`SessionState`].
//!
//! Layering rule: this crate must never depend on `rho-ai` or `rho-coding`.
//! Cargo's acyclic dependency graph enforces what tau could only document,
//! dissolving the historical `tau_agent` ↔ `tau_ai` import cycle.
//!
//! Milestone M0 ships this crate as an empty scaffold; the wire types land in M1.
