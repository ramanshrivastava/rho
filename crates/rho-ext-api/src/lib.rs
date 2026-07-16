//! `rho-ext-api` — the extension guest API surface.
//!
//! This crate defines the types and entry points an extension author links
//! against when building a rho extension to WebAssembly: the hook signatures,
//! the custom-message and UI-bridge contracts, and the serializable payloads
//! exchanged with the host in [`rho_ext_host`]. It is the Rust counterpart to
//! tau's `tau_coding.extensions.api` module.
//!
//! Milestone M0 ships this crate as an intentionally empty stub; the guest API is
//! defined alongside the host in M7.
