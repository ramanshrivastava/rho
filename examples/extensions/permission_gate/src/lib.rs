//! rho extension that blocks dangerous bash commands before they run.
//!
//! Ported from tau's `examples/extensions/permission_gate.py` — the `tool_call`
//! hook returns a blocking outcome and the tool never executes; the model sees
//! the block reason instead. The guarded patterns and block-reason text match
//! tau exactly. Build with:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2 \
//!     --manifest-path examples/extensions/permission_gate/Cargo.toml
//! ```

use std::sync::OnceLock;

use fancy_regex::Regex;
use rho_ext_api::prelude::*;

/// The guarded patterns, verbatim from tau's `DANGEROUS_PATTERNS`.
const DANGEROUS_PATTERNS: &[&str] = &[
    // rm with a flag cluster containing both r and f, in either order
    r"\brm\s+-(?=[a-zA-Z]*r)(?=[a-zA-Z]*f)[a-zA-Z]+",
    r"\bgit\s+push\s+--force",
    r"\bgit\s+reset\s+--hard",
    r"\bchmod\s+-R\s+777\b",
    r"\bdd\s+if=",
    r"\bmkfs\b",
];

fn compiled() -> &'static Vec<Regex> {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        DANGEROUS_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("guarded pattern must compile"))
            .collect()
    })
}

struct PermissionGate;

impl Extension for PermissionGate {
    fn setup(rho: &mut Setup) {
        rho.on_tool_call(|event| {
            if event.tool_name != "bash" {
                return None;
            }
            let command = event
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("");
            for (regex, pattern) in compiled().iter().zip(DANGEROUS_PATTERNS) {
                if regex.is_match(command).unwrap_or(false) {
                    return Some(ToolCallOutcome::block(format!(
                        "command matches guarded pattern `{pattern}`; \
                         ask the user to run it manually if it is intended"
                    )));
                }
            }
            None
        });
    }
}

export_extension!(PermissionGate);
