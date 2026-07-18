//! Minimal rho extension: one custom tool `hello`.
//!
//! Ported from tau's `examples/extensions/hello_tool.py`. Build with:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2 \
//!     --manifest-path examples/extensions/hello_tool/Cargo.toml
//! ```

use rho_ext_api::prelude::*;

struct HelloTool;

impl Extension for HelloTool {
    fn setup(rho: &mut Setup) {
        rho.tool(
            ToolDef::new("hello", "Greet someone by name.")
                .parameters(json!({
                    "type": "object",
                    "properties": {
                        "who": {"type": "string", "description": "Who to greet."}
                    }
                }))
                .prompt_snippet("Greet someone by name."),
            |args| {
                let who = args.get("who").and_then(Value::as_str).unwrap_or("world");
                ToolResult::text(format!("Hello, {who}!"))
            },
        );
    }
}

export_extension!(HelloTool);
