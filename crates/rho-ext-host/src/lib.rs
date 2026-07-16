//! `rho-ext-host` — the WebAssembly extension host.
//!
//! This crate will embed a [`wasmtime`](https://docs.rs/wasmtime) runtime to load,
//! sandbox, and drive rho extensions (the Rust analogue of tau's
//! `tau_coding.extensions` runtime), exposing the guest-facing surface defined by
//! [`rho_ext_api`] to compiled extension modules.
//!
//! Milestone M0 ships this crate as an intentionally empty stub. The `wasmtime`
//! dependency is deliberately **not** added yet — it lands with the real host in
//! M7 to keep the M0 workspace fast to build.
