//! Shared low-level JSON types for the portable agent layer.
//!
//! Port of tau's `tau_agent/types.py`. Pydantic used a PEP 695 recursive alias
//! (`type JSONValue = ... | list[JSONValue] | dict[str, JSONValue]`) for
//! arbitrary JSON payloads (tool `arguments`, tool-result `details`, custom
//! `data`). Rust's `serde_json::Value` is exactly that recursive sum type, so we
//! alias to it rather than re-deriving a bespoke enum.
//!
//! Two properties make `serde_json::Value` the correct oracle-preserving choice:
//!
//! * With the workspace's `preserve_order` feature, its object variant is an
//!   `IndexMap`, so key insertion order survives a parse → serialize round-trip
//!   (tau relies on this — see the legacy `{**data, **details}` merge).
//! * `exclude_none` in tau does **not** recurse into these free-form values, so a
//!   literal `null` *inside* `arguments`/`details`/`data` must be preserved.
//!   `Value::Null` nested inside a `Value` is serialized verbatim; only *typed*
//!   `Option` fields are skipped when `None`.

/// Arbitrary JSON value (tau's `JSONValue`).
pub type JsonValue = serde_json::Value;

/// A JSON object with insertion-order-preserving keys (tau's `dict[str, JSONValue]`).
pub type JsonMap = serde_json::Map<String, serde_json::Value>;
