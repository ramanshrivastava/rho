//! Shared fixtures for the golden tests: the `read_file`/`bash` tools, the user
//! messages, and a Codex credential resolver — reconstructed to match
//! `tools/extract-fixtures/extract_sse.py` exactly.

#![allow(dead_code)]

use std::sync::Arc;

use futures::future::BoxFuture;
use rho_agent::messages::{AgentMessage, UserMessage};
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolExecutor};
use rho_ai::env::{OpenAICodexCredentialResolver, OpenAICodexCredentials};
use serde_json::Value;

/// A no-op tool executor (the extraction's tools return an empty result).
fn noop_executor() -> ToolExecutor {
    Arc::new(
        |_id,
         _args,
         _signal,
         _on_update|
         -> BoxFuture<'static, Result<AgentToolResult, ToolError>> {
            Box::pin(async { Ok(AgentToolResult::new(Vec::new())) })
        },
    )
}

fn tool(name: &str, description: &str, params: Value) -> AgentTool {
    let parameters = match params {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    AgentTool::new(name, name, description, parameters, noop_executor())
}

/// The extraction's `READ_TOOL`.
pub fn read_tool() -> AgentTool {
    tool(
        "read_file",
        "Read a file",
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
}

/// The extraction's `BASH_TOOL`.
pub fn bash_tool() -> AgentTool {
    tool(
        "bash",
        "Run a shell command",
        serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
    )
}

/// A user message (the extraction stamps a timestamp that never reaches output).
pub fn user(content: &str) -> AgentMessage {
    AgentMessage::User(UserMessage::new(content))
}

/// The extraction's `_codex_creds` resolver.
pub fn codex_creds_resolver() -> OpenAICodexCredentialResolver {
    Arc::new(
        || -> BoxFuture<'static, Result<OpenAICodexCredentials, String>> {
            Box::pin(async {
                Ok(OpenAICodexCredentials {
                    access_token: "access-token".to_string(),
                    account_id: "account-1".to_string(),
                })
            })
        },
    )
}
