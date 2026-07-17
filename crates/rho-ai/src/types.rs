//! Small shared type aliases for the provider layer.

/// Arbitrary JSON value (re-exported from the agent core).
pub use rho_agent::types::{JsonMap, JsonValue};

/// An ordered list of request headers (tau's `Mapping[str, str]`, order kept for
/// the live/mock HTTP path). Header order does not affect the golden request
/// *bodies* — only the JSON payload is asserted byte-for-byte — but preserving
/// insertion order keeps the wire close to tau's dict iteration.
pub type HeaderList = Vec<(String, String)>;
