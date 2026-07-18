//! End-to-end integration: the real `WasmExtensionHost` driven through
//! `rho-coding`'s `ExtensionRuntime` (the seam a live `CodingSession` uses).
//!
//! This closes the loop between the two cluster test suites — `rho-ext-host`
//! proves the wasmtime host runs guests, and the `ExtensionRuntime` unit tests
//! prove the tau-parity orchestration against a fake host. Here the *real*
//! wasmtime host is selected by `ExtensionRuntime::for_session()` (feature
//! `wasmtime`), a compiled example guest is discovered, and the composed tools
//! are executed — exactly the path `CodingSession::load` takes with `-x`.
//!
//! Requires the `wasm32-wasip2` target (the repo dev setup installs it).

#![cfg(feature = "wasmtime")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};

use rho_agent::messages::{TextContent, ToolResultContent};
use rho_agent::tools::{AgentTool, AgentToolResult, ToolExecutor};
use rho_coding::extensions::ExtensionRuntime;
use rho_ext_host::ExtensionSpec;
use serde_json::json;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root")
}

/// Build the example guests once per test-binary run and return a component path.
fn guest_component(name: &str) -> PathBuf {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        for guest in ["hello_tool", "permission_gate"] {
            let manifest = repo_root()
                .join("examples/extensions")
                .join(guest)
                .join("Cargo.toml");
            let status = Command::new(env!("CARGO"))
                .args([
                    "build",
                    "--release",
                    "--target",
                    "wasm32-wasip2",
                    "--manifest-path",
                ])
                .arg(&manifest)
                .status()
                .expect("spawn cargo to build guest");
            assert!(status.success(), "building guest `{guest}` failed");
        }
    });
    let path = repo_root()
        .join("examples/extensions")
        .join(name)
        .join("target/wasm32-wasip2/release")
        .join(format!("{name}.wasm"));
    assert!(path.is_file(), "missing guest: {}", path.display());
    path
}

fn spec(name: &str) -> ExtensionSpec {
    ExtensionSpec {
        name: name.to_string(),
        path: guest_component(name),
    }
}

/// A minimal built-in `bash` tool whose executor records nothing and returns
/// "ran" — the permission-gate guest should block dangerous calls before it.
fn bash_tool() -> AgentTool {
    let execute: ToolExecutor = Arc::new(|_id, _args, _sig, _upd| {
        Box::pin(async move {
            Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                TextContent::new("ran"),
            )]))
        })
    });
    AgentTool::new(
        "bash",
        "bash",
        "run a shell command",
        serde_json::Map::new(),
        execute,
    )
}

#[tokio::test]
async fn hello_tool_composes_and_executes_through_the_session_runtime() {
    let mut runtime = ExtensionRuntime::for_session();
    runtime.load_discovered(vec![spec("hello_tool")]).await;

    // No error diagnostics from a real load.
    assert!(
        runtime.diagnostics().iter().all(|d| d.severity != "error"),
        "unexpected error diagnostics: {:?}",
        runtime.diagnostics()
    );

    let tools = runtime.compose_tools(vec![]);
    let hello = tools
        .iter()
        .find(|t| t.name == "hello")
        .expect("`hello` tool registered by the guest");

    let greeted = hello
        .execute(
            "call-1".to_string(),
            json!({"who": "Ada"}).as_object().unwrap().clone(),
            None,
            no_update(),
        )
        .await
        .expect("hello executes");
    assert_eq!(greeted.text(), "Hello, Ada!");

    let default = hello
        .execute(
            "call-2".to_string(),
            serde_json::Map::new(),
            None,
            no_update(),
        )
        .await
        .expect("hello executes");
    assert_eq!(default.text(), "Hello, world!");
}

#[tokio::test]
async fn permission_gate_blocks_dangerous_bash_through_the_session_runtime() {
    let mut runtime = ExtensionRuntime::for_session();
    runtime.load_discovered(vec![spec("permission_gate")]).await;

    let tools = runtime.compose_tools(vec![bash_tool()]);
    let bash = tools
        .iter()
        .find(|t| t.name == "bash")
        .expect("bash composed");

    let blocked = bash
        .execute(
            "c".to_string(),
            json!({"command": "rm -rf build/"})
                .as_object()
                .unwrap()
                .clone(),
            None,
            no_update(),
        )
        .await
        .expect("wrapped bash runs the hook");
    assert!(
        blocked.text().to_lowercase().contains("blocked"),
        "dangerous command should be blocked, got: {}",
        blocked.text()
    );

    let allowed = bash
        .execute(
            "c2".to_string(),
            json!({"command": "ls -la"}).as_object().unwrap().clone(),
            None,
            no_update(),
        )
        .await
        .expect("wrapped bash runs the hook");
    assert_eq!(allowed.text(), "ran", "safe command should pass through");
}

fn no_update() -> rho_agent::tools::ToolUpdateCallback {
    Arc::new(|_result| {})
}
