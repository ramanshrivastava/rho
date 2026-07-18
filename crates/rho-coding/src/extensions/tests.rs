//! Parity tests for the extension runtime, ported from tau's
//! `tests/test_extensions.py` and `tests/test_example_extensions.py`.
//!
//! The orchestration is exercised through [`FakeExtensionHost`] (Rust-closure
//! extensions) rather than a WASM toolchain, and discovery through
//! `rho_ext_host::discover_extensions`. See the crate report for the tau tests
//! that are N/A under the WASM one-file model or the frozen trait.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use rho_agent::events::{AgentEvent, AgentStartEvent, TurnEndEvent, TurnStartEvent};
use rho_agent::messages::{AgentMessage, TextContent, ToolResultContent, UserMessage};
use rho_agent::tools::{AgentTool, AgentToolResult, ToolExecutor, ToolUpdateCallback};
use rho_agent::types::JsonMap;

use rho_ext_host::{
    CommandDef, DiscoveryPaths, ExtensionSpec, HostError, InputAction, InputOutcome,
    ToolCallOutcome, ToolCallResult, ToolDef, ToolResultOutcome, discover_extensions,
};

use super::ExtensionRuntime;
use super::fake_host::{FakeExtension, FakeExtensionHost};
use crate::resources::RhoResourcePaths;
use crate::session::{CodingSession, CodingSessionConfig, jsonl_session_storage};

// -- helpers ------------------------------------------------------------------

fn spec(name: &str) -> ExtensionSpec {
    ExtensionSpec {
        name: name.to_string(),
        path: PathBuf::from(format!("/virtual/{name}.wasm")),
    }
}

fn tool_def(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        label: name.to_string(),
        description: "d".to_string(),
        parameters: json!({}),
        prompt_snippet: None,
    }
}

fn noop_update() -> ToolUpdateCallback {
    Arc::new(|_| {})
}

async fn execute(tool: &AgentTool, arguments: JsonMap) -> AgentToolResult {
    tool.execute("call-1".to_string(), arguments, None, noop_update())
        .await
        .expect("tool executed")
}

fn map(value: Value) -> JsonMap {
    match value {
        Value::Object(map) => map,
        _ => JsonMap::new(),
    }
}

/// A built-in-style tool implemented as a Rust closure returning `content`.
fn builtin_tool(name: &str, content: &str) -> AgentTool {
    let content = content.to_string();
    let execute: ToolExecutor = Arc::new(move |_id, _args, _sig, _upd| {
        let content = content.clone();
        Box::pin(async move {
            Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                TextContent::new(content),
            )]))
        })
    });
    AgentTool::new(name, name, "d", JsonMap::new(), execute)
}

/// A tool that records the arguments it was called with.
fn recording_tool(name: &str) -> (AgentTool, Arc<Mutex<Vec<JsonMap>>>) {
    let seen: Arc<Mutex<Vec<JsonMap>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let execute: ToolExecutor = Arc::new(move |_id, args, _sig, _upd| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(args.clone());
            Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                TextContent::new("ran"),
            )]))
        })
    });
    (
        AgentTool::new(name, name, "d", JsonMap::new(), execute),
        seen,
    )
}

async fn runtime_with(
    extensions: impl IntoIterator<Item = FakeExtension>,
    names: &[&str],
) -> ExtensionRuntime {
    let host = FakeExtensionHost::with(extensions);
    let mut runtime = ExtensionRuntime::with_host(host);
    runtime
        .load_discovered(names.iter().map(|name| spec(name)).collect())
        .await;
    runtime
}

fn has_diagnostic(runtime: &ExtensionRuntime, needle: &str) -> bool {
    runtime
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.message.contains(needle))
}

// -- registration & composition ----------------------------------------------

#[tokio::test]
async fn extension_tool_registration_and_composition() {
    let ext = FakeExtension::new("hello_ext").with_tool(tool_def("hello"), |_| {
        Ok(ToolCallResult {
            text: "hi".to_string(),
            details: None,
        })
    });
    let runtime = runtime_with([ext], &["hello_ext"]).await;

    let composed = runtime.compose_tools(vec![]);
    assert_eq!(
        composed.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        vec!["hello".to_string()]
    );
}

#[tokio::test]
async fn extension_tool_overrides_builtin_by_name() {
    let ext = FakeExtension::new("override").with_tool(tool_def("read"), |_| {
        Ok(ToolCallResult {
            text: "intercepted".to_string(),
            details: None,
        })
    });
    let runtime = runtime_with([ext], &["override"]).await;

    let composed = runtime.compose_tools(vec![builtin_tool("read", "builtin")]);
    assert_eq!(composed.len(), 1);
    assert_eq!(composed[0].name, "read");
    let result = execute(&composed[0], JsonMap::new()).await;
    assert_eq!(result.text(), "intercepted");
}

fn hello_returning(name: &str) -> FakeExtension {
    FakeExtension::new(name).with_tool(tool_def("hello"), |_| {
        Ok(ToolCallResult {
            text: "hi".to_string(),
            details: None,
        })
    })
}

#[tokio::test]
async fn duplicate_tool_registration_first_wins() {
    let runtime = runtime_with(
        [hello_returning("ext_one"), hello_returning("ext_two")],
        &["ext_one", "ext_two"],
    )
    .await;

    assert_eq!(runtime.extension_tools().len(), 1);
    assert!(has_diagnostic(&runtime, "already registered"));
}

// -- tool_call / tool_result hooks -------------------------------------------

#[tokio::test]
async fn tool_call_hook_can_block() {
    let ext = FakeExtension::new("guard").on_tool_call(|event| {
        Ok((event.tool_name == "danger").then(|| ToolCallOutcome {
            block: true,
            reason: Some("not allowed".to_string()),
            arguments: None,
        }))
    });
    let runtime = runtime_with([ext], &["guard"]).await;

    let wrapped = runtime.compose_tools(vec![builtin_tool("danger", "ran")]);
    let result = execute(&wrapped[0], JsonMap::new()).await;
    assert!(result.text().starts_with("Tool call blocked:"));
    assert!(result.text().contains("not allowed"));
}

#[tokio::test]
async fn tool_call_hook_can_rewrite_arguments() {
    let ext = FakeExtension::new("rewrite").on_tool_call(|_| {
        Ok(Some(ToolCallOutcome {
            block: false,
            reason: None,
            arguments: Some(json!({"who": "tau"})),
        }))
    });
    let runtime = runtime_with([ext], &["rewrite"]).await;
    let (echo, seen) = recording_tool("echo");

    let wrapped = runtime.compose_tools(vec![echo]);
    execute(&wrapped[0], map(json!({"who": "world"}))).await;
    assert_eq!(&*seen.lock().unwrap(), &[map(json!({"who": "tau"}))]);
}

#[tokio::test]
async fn tool_call_hook_can_clear_arguments() {
    let ext = FakeExtension::new("clearer").on_tool_call(|_| {
        Ok(Some(ToolCallOutcome {
            block: false,
            reason: None,
            arguments: Some(json!({})),
        }))
    });
    let runtime = runtime_with([ext], &["clearer"]).await;
    let (echo, seen) = recording_tool("echo");

    let wrapped = runtime.compose_tools(vec![echo]);
    execute(&wrapped[0], map(json!({"who": "world"}))).await;
    assert_eq!(&*seen.lock().unwrap(), &[JsonMap::new()]);
}

#[tokio::test]
async fn raising_tool_call_hook_blocks_fail_safe() {
    let ext = FakeExtension::new("raiser")
        .on_tool_call(|_| Err(HostError::dispatch("raiser", "tool_call", "hook exploded")));
    let runtime = runtime_with([ext], &["raiser"]).await;

    let wrapped = runtime.compose_tools(vec![builtin_tool("x", "ran")]);
    let result = execute(&wrapped[0], JsonMap::new()).await;
    assert!(result.text().starts_with("Tool call blocked:"));
    assert!(result.text().contains("hook failed"));
    assert!(has_diagnostic(&runtime, "tool_call"));
}

#[tokio::test]
async fn tool_result_hook_transforms_result() {
    let ext = FakeExtension::new("transform").on_tool_result(|_| {
        Ok(Some(ToolResultOutcome {
            content: Some("redacted".to_string()),
            details: None,
        }))
    });
    let runtime = runtime_with([ext], &["transform"]).await;

    let wrapped = runtime.compose_tools(vec![builtin_tool("x", "secret")]);
    let result = execute(&wrapped[0], JsonMap::new()).await;
    assert_eq!(result.text(), "redacted");
}

#[tokio::test]
async fn raising_tool_result_hook_keeps_result() {
    let ext = FakeExtension::new("raiser")
        .on_tool_result(|_| Err(HostError::dispatch("raiser", "tool_result", "boom")));
    let runtime = runtime_with([ext], &["raiser"]).await;

    let wrapped = runtime.compose_tools(vec![builtin_tool("x", "fine")]);
    let result = execute(&wrapped[0], JsonMap::new()).await;
    assert_eq!(result.text(), "fine");
    assert!(has_diagnostic(&runtime, "tool_result"));
}

#[tokio::test]
async fn wrapped_tool_forwards_on_update() {
    let ext = FakeExtension::new("progress");
    let runtime = runtime_with([ext], &["progress"]).await;

    let execute_fn: ToolExecutor = Arc::new(|_id, _args, _sig, on_update| {
        Box::pin(async move {
            on_update(AgentToolResult::new(vec![ToolResultContent::Text(
                TextContent::new("halfway"),
            )]));
            Ok(AgentToolResult::new(vec![ToolResultContent::Text(
                TextContent::new("done"),
            )]))
        })
    });
    let tool = AgentTool::new("work", "work", "d", JsonMap::new(), execute_fn);
    let wrapped = runtime.compose_tools(vec![tool]);

    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    let on_update: ToolUpdateCallback =
        Arc::new(move |result| sink.lock().unwrap().push(result.text()));
    let result = wrapped[0]
        .execute("call-1".to_string(), JsonMap::new(), None, on_update)
        .await
        .unwrap();

    assert_eq!(result.text(), "done");
    assert_eq!(&*received.lock().unwrap(), &["halfway".to_string()]);
}

// -- input hooks --------------------------------------------------------------

#[tokio::test]
async fn input_hooks_chain_transforms() {
    let one = FakeExtension::new("one").on_input(|event| {
        Ok(Some(InputOutcome {
            action: InputAction::Transform,
            text: Some(format!("{} one", event.text)),
            message: None,
        }))
    });
    let two = FakeExtension::new("two").on_input(|event| {
        Ok(Some(InputOutcome {
            action: InputAction::Transform,
            text: Some(format!("{} two", event.text)),
            message: None,
        }))
    });
    let runtime = runtime_with([one, two], &["one", "two"]).await;

    let outcome = runtime.run_input_hooks("base", "interactive", None).await;
    assert!(!outcome.handled);
    assert_eq!(outcome.text, "base one two");
}

#[tokio::test]
async fn input_hook_handled_short_circuits() {
    let handler = FakeExtension::new("handler").on_input(|_| {
        Ok(Some(InputOutcome {
            action: InputAction::Handled,
            text: None,
            message: Some("done".to_string()),
        }))
    });
    let never = FakeExtension::new("never").on_input(|_| {
        Ok(Some(InputOutcome {
            action: InputAction::Transform,
            text: Some("never".to_string()),
            message: None,
        }))
    });
    let runtime = runtime_with([handler, never], &["handler", "never"]).await;

    let outcome = runtime.run_input_hooks("base", "interactive", None).await;
    assert!(outcome.handled);
    assert_eq!(outcome.message.as_deref(), Some("done"));
}

#[tokio::test]
async fn input_hook_receives_source_and_streaming_behavior() {
    let seen = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let sink = seen.clone();
    let ext = FakeExtension::new("capture").on_input(move |event| {
        sink.lock()
            .unwrap()
            .push((event.source.clone(), event.streaming_behavior.clone()));
        Ok(None)
    });
    let runtime = runtime_with([ext], &["capture"]).await;

    runtime
        .run_input_hooks("go", "extension", Some("steer".to_string()))
        .await;
    assert_eq!(
        &*seen.lock().unwrap(),
        &[("extension".to_string(), Some("steer".to_string()))]
    );
}

// -- agent event fan-out ------------------------------------------------------

#[tokio::test]
async fn agent_event_fan_out_and_wildcard() {
    let specific: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let wildcard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = specific.clone();
    let w = wildcard.clone();
    let ext = FakeExtension::new("observer")
        .on_agent_event("tool_execution_start", move |event| {
            s.lock().unwrap().push(event.event_type.clone());
            Ok(())
        })
        .on_any_agent_event(move |event| {
            w.lock().unwrap().push(event.event_type.clone());
            Ok(())
        });
    let mut runtime = runtime_with([ext], &["observer"]).await;

    runtime
        .emit_event(
            "tool_execution_start",
            json!({"type": "tool_execution_start"}),
        )
        .await;
    runtime
        .on_agent_event(&AgentEvent::TurnStart(TurnStartEvent::new()))
        .await;

    assert_eq!(specific.lock().unwrap().len(), 1);
    assert_eq!(wildcard.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn extension_turn_events_include_pi_session_metadata() {
    let events: Arc<Mutex<Vec<(String, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let ext = FakeExtension::new("turn_observer").on_any_agent_event(move |event| {
        let index = event
            .payload
            .get("turn_index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        sink.lock().unwrap().push((event.event_type.clone(), index));
        Ok(())
    });
    let mut runtime = runtime_with([ext], &["turn_observer"]).await;

    let message = AgentMessage::User(UserMessage::new("done"));
    let before = super::now_millis();
    runtime
        .on_agent_event(&AgentEvent::AgentStart(AgentStartEvent::new()))
        .await;
    runtime
        .on_agent_event(&AgentEvent::TurnStart(TurnStartEvent::new()))
        .await;
    runtime
        .on_agent_event(&AgentEvent::TurnEnd(TurnEndEvent::new(message, vec![])))
        .await;
    runtime
        .on_agent_event(&AgentEvent::TurnStart(TurnStartEvent::new()))
        .await;

    let seen = events.lock().unwrap().clone();
    // agent_start, turn_start(0), turn_end(0), turn_start(1)
    assert_eq!(seen[1], ("turn_start".to_string(), 0));
    assert_eq!(seen[2], ("turn_end".to_string(), 0));
    assert_eq!(seen[3], ("turn_start".to_string(), 1));
    assert!(before <= super::now_millis());
}

#[tokio::test]
async fn extension_turn_index_resets_for_each_agent_run() {
    let indices: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = indices.clone();
    let ext = FakeExtension::new("turn_observer").on_agent_event("turn_start", move |event| {
        sink.lock().unwrap().push(
            event
                .payload
                .get("turn_index")
                .and_then(Value::as_u64)
                .unwrap(),
        );
        Ok(())
    });
    let mut runtime = runtime_with([ext], &["turn_observer"]).await;

    for _ in 0..2 {
        runtime
            .on_agent_event(&AgentEvent::AgentStart(AgentStartEvent::new()))
            .await;
        runtime
            .on_agent_event(&AgentEvent::TurnStart(TurnStartEvent::new()))
            .await;
        runtime
            .on_agent_event(&AgentEvent::TurnEnd(TurnEndEvent::new(
                AgentMessage::User(UserMessage::new("done")),
                vec![],
            )))
            .await;
    }

    assert_eq!(&*indices.lock().unwrap(), &[0, 0]);
}

#[tokio::test]
async fn raising_event_handler_is_recorded() {
    let ext = FakeExtension::new("raiser").on_agent_event("turn_start", |_| {
        Err(HostError::dispatch("raiser", "turn_start", "boom"))
    });
    let mut runtime = runtime_with([ext], &["raiser"]).await;

    runtime
        .on_agent_event(&AgentEvent::TurnStart(TurnStartEvent::new()))
        .await;
    assert!(has_diagnostic(&runtime, "turn_start"));
}

// -- prompt guidelines --------------------------------------------------------

#[tokio::test]
async fn prompt_guideline_registration() {
    let ext = FakeExtension::new("guidance")
        .with_guideline("Always run the tests before claiming success")
        .with_guideline("   ");
    let runtime = runtime_with([ext], &["guidance"]).await;

    assert_eq!(
        runtime.prompt_guidelines(),
        vec!["Always run the tests before claiming success".to_string()]
    );
    assert!(has_diagnostic(&runtime, "empty prompt guideline"));
}

// -- commands -----------------------------------------------------------------

#[tokio::test]
async fn extension_commands_layer_onto_default_registry() {
    let ext = FakeExtension::new("cmd_ext").with_command(CommandDef {
        name: "echo".to_string(),
        description: "Echo args.".to_string(),
        usage: None,
        aliases: vec![],
    });
    let runtime = runtime_with([ext], &["cmd_ext"]).await;

    let registry = runtime.build_command_registry();
    let command = registry.get("echo").expect("echo command registered");
    assert_eq!(command.description, "Echo args.");
    // Execution is deferred (no call-command in the WIT); the placeholder
    // handler reports the command is not executable in this build.
}

#[tokio::test]
async fn extension_command_cannot_shadow_builtin() {
    let ext = FakeExtension::new("shadow").with_command(CommandDef {
        name: "model".to_string(),
        description: "hijack".to_string(),
        usage: None,
        aliases: vec![],
    });
    let runtime = runtime_with([ext], &["shadow"]).await;

    let registry = runtime.build_command_registry();
    let command = registry.get("model").expect("builtin model present");
    assert_eq!(command.description, "Choose the active model.");
    assert!(has_diagnostic(&runtime, "could not register command"));
}

// -- message renderers --------------------------------------------------------

#[tokio::test]
async fn render_custom_message_uses_registered_renderer() {
    let ext = FakeExtension::new("notifier").with_renderer(
        "subagent-notification",
        |_ct, content, details, expanded| {
            let label = details
                .and_then(|d| d.get("label"))
                .and_then(Value::as_str)
                .unwrap_or(content);
            Ok(Some(format!("[bold]{label}[/bold] {expanded}")))
        },
    );
    let runtime = runtime_with([ext], &["notifier"]).await;

    let markup = runtime
        .render_custom_message(
            "subagent-notification",
            "raw",
            Some(&json!({"label": "done"})),
            true,
        )
        .await;
    assert_eq!(markup.as_deref(), Some("[bold]done[/bold] true"));
}

#[tokio::test]
async fn render_custom_message_returns_none_when_unregistered() {
    let runtime = runtime_with([FakeExtension::new("notifier")], &["notifier"]).await;
    assert!(
        runtime
            .render_custom_message("unknown", "raw", None, false)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn register_message_renderer_first_registration_wins() {
    let first = FakeExtension::new("first")
        .with_renderer("shared", |_, _, _, _| Ok(Some("first".to_string())));
    let second = FakeExtension::new("second")
        .with_renderer("shared", |_, _, _, _| Ok(Some("second".to_string())));
    let runtime = runtime_with([first, second], &["first", "second"]).await;

    let markup = runtime
        .render_custom_message("shared", "x", None, false)
        .await;
    assert_eq!(markup.as_deref(), Some("first"));
}

#[tokio::test]
async fn render_custom_message_swallows_errors_and_reports_once() {
    let ext = FakeExtension::new("boom").with_renderer("boom", |_, _, _, _| {
        Err(HostError::dispatch("boom", "render", "exploded"))
    });
    let runtime = runtime_with([ext], &["boom"]).await;

    for _ in 0..5 {
        assert!(
            runtime
                .render_custom_message("boom", "raw", None, false)
                .await
                .is_none()
        );
    }
    let failures = runtime
        .diagnostics()
        .into_iter()
        .filter(|d| d.message.contains("message_renderer:boom"))
        .count();
    assert_eq!(failures, 1);
}

// -- reload -------------------------------------------------------------------

#[tokio::test]
async fn reload_picks_up_changes() {
    let host = Arc::new(FakeExtensionHost::new());
    let mut runtime = ExtensionRuntime::with_host(host.clone());

    // First load: an extension with just a guideline.
    host.insert(FakeExtension::new("late").with_guideline("Prefer uv over pip"));
    runtime.load_discovered(vec![spec("late")]).await;
    assert_eq!(
        runtime.prompt_guidelines(),
        vec!["Prefer uv over pip".to_string()]
    );
    assert!(runtime.compose_tools(vec![]).is_empty());

    // Reconfigure the host and reload: a new tool appears, guideline gone.
    runtime.reset_for_reload().await;
    host.insert(
        FakeExtension::new("late").with_tool(tool_def("hello"), |_| {
            Ok(ToolCallResult {
                text: "hi".to_string(),
                details: None,
            })
        }),
    );
    runtime.load_discovered(vec![spec("late")]).await;

    assert!(runtime.prompt_guidelines().is_empty());
    assert_eq!(
        runtime
            .compose_tools(vec![])
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>(),
        vec!["hello".to_string()]
    );
}

// -- example-extension semantics (mirrors test_example_extensions.py) --------

fn hello_extension() -> FakeExtension {
    FakeExtension::new("hello_tool").with_tool(
        ToolDef {
            name: "hello".to_string(),
            label: "hello".to_string(),
            description: "Greet someone by name.".to_string(),
            parameters: json!({"type": "object", "properties": {"who": {"type": "string"}}}),
            prompt_snippet: Some("Greet someone by name.".to_string()),
        },
        |arguments| {
            let who = arguments
                .get("who")
                .and_then(Value::as_str)
                .unwrap_or("world");
            Ok(ToolCallResult {
                text: format!("Hello, {who}!"),
                details: None,
            })
        },
    )
}

const DANGEROUS_PATTERNS: &[&str] = &[
    r"\brm\s+-(?=[a-zA-Z]*r)(?=[a-zA-Z]*f)[a-zA-Z]+",
    r"\bgit\s+push\s+--force",
    r"\bgit\s+reset\s+--hard",
    r"\bchmod\s+-R\s+777\b",
    r"\bdd\s+if=",
    r"\bmkfs\b",
];

fn permission_gate_extension() -> FakeExtension {
    FakeExtension::new("permission_gate").on_tool_call(|event| {
        if event.tool_name != "bash" {
            return Ok(None);
        }
        let command = event.arguments.get("command").and_then(Value::as_str).unwrap_or("");
        for pattern in DANGEROUS_PATTERNS {
            // `fancy_regex` is not a dep; the `rm -rf` lookahead is emulated by a
            // membership check when the plain crate cannot compile the pattern.
            if pattern_matches(pattern, command) {
                return Ok(Some(ToolCallOutcome {
                    block: true,
                    reason: Some(format!(
                        "command matches guarded pattern `{pattern}`; ask the user to run it manually if it is intended"
                    )),
                    arguments: None,
                }));
            }
        }
        Ok(None)
    })
}

/// Match a guarded pattern. The `rm -rf` rule uses a lookahead the default
/// `regex` crate rejects, so it is emulated directly; the rest compile normally.
fn pattern_matches(pattern: &str, command: &str) -> bool {
    if pattern.contains("(?=") {
        // `\brm\s+-[flags]` where flags contain both r and f (either order).
        if let Some(rest) = rm_flags(command) {
            return rest.contains('r') && rest.contains('f');
        }
        return false;
    }
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(command))
}

fn rm_flags(command: &str) -> Option<&str> {
    let re = regex::Regex::new(r"\brm\s+-([a-zA-Z]+)").ok()?;
    re.captures(command)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

#[tokio::test]
async fn hello_tool_greets() {
    let runtime = runtime_with([hello_extension()], &["hello_tool"]).await;
    let hello = &runtime.compose_tools(vec![])[0];
    assert_eq!(
        execute(hello, map(json!({"who": "Rho"}))).await.text(),
        "Hello, Rho!"
    );
    assert_eq!(execute(hello, JsonMap::new()).await.text(), "Hello, world!");
}

#[tokio::test]
async fn permission_gate_blocks_dangerous_bash() {
    let commands = [
        "rm -rf build/",
        "rm -fr /tmp/x",
        "git push --force origin main",
        "git reset --hard HEAD~3",
        "chmod -R 777 .",
        "dd if=/dev/zero of=/dev/sda",
    ];
    for command in commands {
        let runtime = runtime_with([permission_gate_extension()], &["permission_gate"]).await;
        let (bash, executed) = recording_tool("bash");
        let wrapped = runtime.compose_tools(vec![bash]);
        let result = execute(&wrapped[0], map(json!({"command": command}))).await;
        assert!(
            result.text().to_lowercase().contains("blocked"),
            "{command}"
        );
        assert!(result.text().contains("guarded pattern"), "{command}");
        assert!(executed.lock().unwrap().is_empty(), "{command}");
    }
}

#[tokio::test]
async fn permission_gate_allows_safe_bash() {
    let commands = [
        "ls -la",
        "rm build/output.txt",
        "git push origin feature",
        "git log --oneline",
    ];
    for command in commands {
        let runtime = runtime_with([permission_gate_extension()], &["permission_gate"]).await;
        let (bash, executed) = recording_tool("bash");
        let wrapped = runtime.compose_tools(vec![bash]);
        let result = execute(&wrapped[0], map(json!({"command": command}))).await;
        assert_eq!(result.text(), "ran", "{command}");
        assert_eq!(executed.lock().unwrap().len(), 1, "{command}");
    }
}

#[tokio::test]
async fn permission_gate_ignores_other_tools() {
    let runtime = runtime_with([permission_gate_extension()], &["permission_gate"]).await;
    let (write, executed) = recording_tool("write");
    let wrapped = runtime.compose_tools(vec![write]);
    let result = execute(
        &wrapped[0],
        map(json!({"content": "rm -rf / would be bad"})),
    )
    .await;
    assert_eq!(result.text(), "ran");
    assert_eq!(executed.lock().unwrap().len(), 1);
}

// -- discovery (rho_ext_host::discover_extensions) ---------------------------

fn touch_wasm(dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.wasm")), b"\0asm").unwrap();
}

fn discovery_paths(root: &std::path::Path, cwd: &std::path::Path) -> DiscoveryPaths {
    DiscoveryPaths {
        root: root.to_path_buf(),
        cwd: Some(cwd.to_path_buf()),
    }
}

#[test]
fn discovers_user_extensions_and_skips_project_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    touch_wasm(&root.join("extensions"), "user_ext");
    touch_wasm(&cwd.join(".rho").join("extensions"), "proj_ext");

    let (discovered, diagnostics) =
        discover_extensions(&discovery_paths(&root, &cwd), &[], true, false);
    assert_eq!(
        discovered
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec!["user_ext".to_string()]
    );
    assert!(diagnostics.is_empty());
}

#[test]
fn project_extensions_load_first_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    touch_wasm(&root.join("extensions"), "ext_a");
    touch_wasm(&cwd.join(".rho").join("extensions"), "ext_b");

    let (discovered, _) = discover_extensions(&discovery_paths(&root, &cwd), &[], true, true);
    assert_eq!(
        discovered
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec!["ext_b".to_string(), "ext_a".to_string()]
    );
}

#[test]
fn duplicate_extension_names_prefer_first_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    touch_wasm(&root.join("extensions"), "dup");
    touch_wasm(&cwd.join(".rho").join("extensions"), "dup");

    let (discovered, diagnostics) =
        discover_extensions(&discovery_paths(&root, &cwd), &[], true, true);
    assert_eq!(discovered.len(), 1);
    assert_eq!(
        discovered[0].path,
        cwd.join(".rho").join("extensions").join("dup.wasm")
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("duplicate extension name"))
    );
}

#[test]
fn explicit_extension_paths_load_even_with_discovery_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    touch_wasm(&root.join("extensions"), "skipped");
    let elsewhere = tmp.path().join("elsewhere");
    touch_wasm(&elsewhere, "explicit");
    let explicit = elsewhere.join("explicit.wasm");

    let (discovered, _) =
        discover_extensions(&discovery_paths(&root, &cwd), &[explicit], false, false);
    assert_eq!(
        discovered
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec!["explicit".to_string()]
    );
}

#[test]
fn missing_explicit_path_is_an_error_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    let missing = tmp.path().join("nope.wasm");

    let (_, diagnostics) =
        discover_extensions(&discovery_paths(&root, &cwd), &[missing], true, false);
    assert!(diagnostics.iter().any(|d| d.is_error));
}

#[test]
fn underscore_files_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    touch_wasm(&root.join("extensions"), "_private");

    let (discovered, _) = discover_extensions(&discovery_paths(&root, &cwd), &[], true, false);
    assert!(discovered.is_empty());
}

// -- coding-session integration ----------------------------------------------

#[tokio::test]
async fn session_exposes_extension_tools_guidelines_and_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    let ext = hello_extension()
        .with_guideline("Never commit directly to main")
        .with_command(CommandDef {
            name: "hi".to_string(),
            description: "Say hi.".to_string(),
            usage: None,
            aliases: vec![],
        });
    let host = FakeExtensionHost::with([ext]);
    let mut runtime = ExtensionRuntime::with_host(host);
    runtime.load_discovered(vec![spec("hello_tool")]).await;

    let provider = Arc::new(rho_agent::fake::FakeProvider::new(vec![]));
    let storage = jsonl_session_storage(tmp.path().join("session.jsonl"));
    let mut config = CodingSessionConfig::new(provider, "fake", storage, cwd.clone());
    config.resource_paths = Some(RhoResourcePaths {
        root: tmp.path().join("home"),
        cwd: Some(cwd.clone()),
        agents_root: Some(tmp.path().join("agents")),
        paths: None,
    });
    config.extension_runtime = Some(runtime);

    let mut session = CodingSession::load(config).await.unwrap();

    let tool_names: Vec<String> = session.tools().iter().map(|t| t.name.clone()).collect();
    assert_eq!(&tool_names[..4], &["read", "write", "edit", "bash"]);
    assert!(tool_names.contains(&"hello".to_string()));
    assert!(
        session
            .system_prompt()
            .contains("Never commit directly to main")
    );

    // The extension command layers onto the default registry.
    let result = session.handle_command("/hi");
    assert!(result.handled);
}

#[test]
fn explicit_directory_path_loads_contained_components() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let cwd = tmp.path().join("project");
    let dir = tmp.path().join("bundle");
    touch_wasm(&dir, "tool_a");

    let (discovered, _) = discover_extensions(&discovery_paths(&root, &cwd), &[dir], false, false);
    assert_eq!(
        discovered
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec!["tool_a".to_string()]
    );
}
