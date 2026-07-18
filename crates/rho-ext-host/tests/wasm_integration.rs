//! End-to-end tests for the wasmtime [`WasmExtensionHost`], exercising compiled
//! example guest components.
//!
//! The guests live under `examples/extensions/*` (each its own detached
//! workspace, built for `wasm32-wasip2`). The helpers below compile them on
//! demand — the `wasm32-wasip2` target must be installed (`rustup target add
//! wasm32-wasip2`), which the repo's dev setup provides.

#![cfg(feature = "wasmtime")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};

use async_trait::async_trait;
use serde_json::json;

use rho_ext_host::host::{ExtensionHost, ExtensionSpec, HostBridge};
use rho_ext_host::payload::{ToolCallEvent, ToolResultEvent};
use rho_ext_host::wasm::WasmExtensionHost;

// --------------------------------------------------------------------------
// Guest build helpers
// --------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    // crates/rho-ext-host -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root")
}

/// Build one example guest to a `wasm32-wasip2` component and return its path.
/// Builds each guest at most once per test binary run.
fn guest_component(name: &str) -> PathBuf {
    static BUILD: Once = Once::new();
    // A single Once guards a full build of all guests so we don't invoke cargo
    // concurrently across parallel tests.
    BUILD.call_once(|| {
        for guest in ["hello_tool", "permission_gate", "sandbox_probe", "runaway"] {
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
                .expect("failed to spawn cargo to build guest");
            assert!(status.success(), "building guest `{guest}` failed");
        }
    });

    let path = repo_root()
        .join("examples/extensions")
        .join(name)
        .join("target/wasm32-wasip2/release")
        .join(format!("{name}.wasm"));
    assert!(
        path.is_file(),
        "guest component missing: {}",
        path.display()
    );
    path
}

fn spec(name: &str) -> ExtensionSpec {
    ExtensionSpec {
        name: name.to_string(),
        path: guest_component(name),
    }
}

// --------------------------------------------------------------------------
// A minimal HostBridge for tests
// --------------------------------------------------------------------------

#[derive(Default)]
struct TestBridge {
    notifications: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl HostBridge for TestBridge {
    async fn cwd(&self) -> String {
        "/tmp/rho-test".to_string()
    }
    async fn model(&self) -> String {
        "test-model".to_string()
    }
    async fn provider_name(&self) -> String {
        "test-provider".to_string()
    }
    async fn session_id(&self) -> Option<String> {
        Some("session-1".to_string())
    }
    async fn system_prompt(&self) -> String {
        "You are a test.".to_string()
    }
    async fn is_running(&self) -> bool {
        false
    }
    async fn transcript_json(&self) -> String {
        "[]".to_string()
    }
    async fn notify(&self, message: &str, level: &str) {
        self.notifications
            .lock()
            .unwrap()
            .push((level.to_string(), message.to_string()));
    }
    async fn ui_select(&self, _title: &str, _options: &[String]) -> Option<String> {
        None
    }
    async fn ui_confirm(&self, _title: &str, _message: &str) -> bool {
        false
    }
    async fn ui_input(&self, _title: &str, _placeholder: &str) -> Option<String> {
        None
    }
    async fn send_user_message(&self, _content: &str, _deliver_as: &str) {}
}

fn bridge() -> Arc<dyn HostBridge> {
    Arc::new(TestBridge::default())
}

/// A runaway guest (`loop {}` in its tool) must trap on the host's per-call fuel
/// budget rather than hanging, and the host must stay usable afterwards.
#[tokio::test]
async fn runaway_guest_traps_on_fuel_exhaustion_and_host_survives() {
    let host = WasmExtensionHost::new().expect("host");
    let outcome = host.load(&[spec("runaway")], bridge()).await;
    assert_eq!(outcome.extensions.len(), 1, "runaway should load");

    // The call must COMPLETE (with a trap error), not hang. A generous timeout
    // guards the test harness itself — fuel exhaustion should return far sooner.
    let called = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        host.call_tool("runaway", "spin", &json!({})),
    )
    .await
    .expect("call_tool must return (fuel trap), not hang");
    assert!(
        called.is_err(),
        "a `loop {{}}` tool must trap on fuel exhaustion, got: {called:?}"
    );

    // Host still alive: a fresh extension loads and runs.
    let outcome = host.load(&[spec("hello_tool")], bridge()).await;
    assert_eq!(outcome.extensions.len(), 1);
    let result = host
        .call_tool("hello_tool", "hello", &json!({}))
        .await
        .expect("host survives the trap");
    assert_eq!(result.text, "Hello, world!");
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[tokio::test]
async fn hello_tool_registers_and_greets() {
    let host = WasmExtensionHost::new().expect("host");
    let outcome = host.load(&[spec("hello_tool")], bridge()).await;

    assert!(
        outcome.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        outcome.diagnostics
    );
    assert_eq!(outcome.extensions.len(), 1);
    let loaded = &outcome.extensions[0];
    assert_eq!(loaded.tools.len(), 1);
    assert_eq!(loaded.tools[0].name, "hello");
    assert_eq!(loaded.tools[0].description, "Greet someone by name.");
    assert_eq!(
        loaded.tools[0].prompt_snippet.as_deref(),
        Some("Greet someone by name.")
    );

    // Default greeting.
    let result = host
        .call_tool("hello_tool", "hello", &json!({}))
        .await
        .expect("call ok");
    assert_eq!(result.text, "Hello, world!");

    // Named greeting.
    let result = host
        .call_tool("hello_tool", "hello", &json!({"who": "Ada"}))
        .await
        .expect("call ok");
    assert_eq!(result.text, "Hello, Ada!");
}

#[tokio::test]
async fn permission_gate_blocks_dangerous_and_allows_safe() {
    let host = WasmExtensionHost::new().expect("host");
    let outcome = host.load(&[spec("permission_gate")], bridge()).await;
    assert!(outcome.diagnostics.is_empty(), "{:?}", outcome.diagnostics);

    let loaded = &outcome.extensions[0];
    assert!(
        loaded.subscriptions.iter().any(|s| s == "tool_call"),
        "expected a tool_call subscription, got {:?}",
        loaded.subscriptions
    );

    // `rm -rf` is blocked with tau's exact reason text.
    let event = ToolCallEvent {
        tool_name: "bash".to_string(),
        arguments: json!({"command": "rm -rf /important"}),
    };
    let outcome = host
        .on_tool_call("permission_gate", &event)
        .await
        .expect("dispatch ok")
        .expect("blocked");
    assert!(outcome.block);
    let reason = outcome.reason.expect("reason");
    assert!(
        reason.contains("guarded pattern")
            && reason.contains("ask the user to run it manually if it is intended"),
        "reason was: {reason}"
    );

    // Other guarded patterns.
    for cmd in [
        "git push --force",
        "git reset --hard",
        "dd if=/dev/zero",
        "mkfs.ext4 /dev/sda",
    ] {
        let ev = ToolCallEvent {
            tool_name: "bash".to_string(),
            arguments: json!({ "command": cmd }),
        };
        let out = host.on_tool_call("permission_gate", &ev).await.expect("ok");
        assert!(out.is_some_and(|o| o.block), "expected block for `{cmd}`");
    }

    // Safe commands pass (no outcome).
    for cmd in ["ls -la", "rm file.txt", "git status", "echo hello"] {
        let ev = ToolCallEvent {
            tool_name: "bash".to_string(),
            arguments: json!({ "command": cmd }),
        };
        let out = host.on_tool_call("permission_gate", &ev).await.expect("ok");
        assert!(out.is_none(), "expected `{cmd}` to be allowed, got {out:?}");
    }

    // Non-bash tools are ignored entirely.
    let ev = ToolCallEvent {
        tool_name: "edit".to_string(),
        arguments: json!({"command": "rm -rf /"}),
    };
    assert!(
        host.on_tool_call("permission_gate", &ev)
            .await
            .expect("ok")
            .is_none()
    );
}

#[tokio::test]
async fn sandbox_denies_filesystem_and_network() {
    let host = WasmExtensionHost::new().expect("host");
    let outcome = host.load(&[spec("sandbox_probe")], bridge()).await;
    assert!(outcome.diagnostics.is_empty(), "{:?}", outcome.diagnostics);

    // Filesystem access is denied: no preopens, so the guest traps and the host
    // surfaces a dispatch error rather than leaking file contents.
    let fs = host
        .call_tool("sandbox_probe", "read_secret", &json!({}))
        .await;
    assert!(fs.is_err(), "sandbox should deny filesystem access: {fs:?}");

    // Network access is denied the same way.
    let net = host
        .call_tool("sandbox_probe", "phone_home", &json!({}))
        .await;
    assert!(net.is_err(), "sandbox should deny network access: {net:?}");

    // The host survived the traps and is still usable.
    let again = host
        .call_tool("sandbox_probe", "read_secret", &json!({}))
        .await;
    assert!(again.is_err(), "host should remain usable after a trap");
}

#[tokio::test]
async fn hot_reload_reinstantiates() {
    let host = WasmExtensionHost::new().expect("host");
    let _ = host.load(&[spec("hello_tool")], bridge()).await;

    // Reload replaces the instance set; the tool still works afterwards.
    let outcome = host.load(&[spec("hello_tool")], bridge()).await;
    assert_eq!(outcome.extensions.len(), 1);
    let result = host
        .call_tool("hello_tool", "hello", &json!({"who": "reload"}))
        .await
        .expect("call ok");
    assert_eq!(result.text, "Hello, reload!");
}

#[tokio::test]
async fn unknown_extension_and_teardown() {
    let host = WasmExtensionHost::new().expect("host");
    let _ = host.load(&[spec("hello_tool")], bridge()).await;

    // A tool_result dispatch to an extension that never subscribed is a no-op
    // (the guest handler set is empty), not an error.
    let ev = ToolResultEvent {
        tool_name: "hello".to_string(),
        arguments: json!({}),
        result_text: "Hello, world!".to_string(),
        result_details: None,
    };
    assert!(
        host.on_tool_result("hello_tool", &ev)
            .await
            .expect("ok")
            .is_none()
    );

    // Unknown extension -> error.
    let err = host.call_tool("nope", "hello", &json!({})).await;
    assert!(err.is_err());

    // Teardown drops instances; dispatch afterwards is UnknownExtension.
    host.teardown().await;
    let err = host.call_tool("hello_tool", "hello", &json!({})).await;
    assert!(err.is_err());
}
