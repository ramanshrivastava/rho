//! Ported from tau `tests/test_coding_tools.py` — the tool behavior tests
//! (metadata, read offset/limit, write, edit match/no-match/duplicate, bash
//! capture / prefix / timeout / process-group kill / cancellation).

use std::sync::Arc;
use std::time::Instant;

use rho_agent::provider::{CancellationToken, SimpleCancellationToken};
use rho_agent::tools::{AgentTool, AgentToolResult, ToolError, ToolUpdateCallback};
use rho_agent::types::JsonMap;

use super::*;

fn noop_update() -> ToolUpdateCallback {
    Arc::new(|_partial: AgentToolResult| {})
}

async fn run(tool: &AgentTool, args: serde_json::Value) -> Result<AgentToolResult, ToolError> {
    run_signal(tool, args, None).await
}

async fn run_signal(
    tool: &AgentTool,
    args: serde_json::Value,
    signal: Option<Arc<dyn CancellationToken>>,
) -> Result<AgentToolResult, ToolError> {
    let map: JsonMap = match args {
        serde_json::Value::Object(m) => m,
        _ => JsonMap::new(),
    };
    tool.execute("test-call".into(), map, signal, noop_update())
        .await
}

#[test]
fn create_coding_tools_returns_initial_tool_set() {
    let dir = tempfile::tempdir().unwrap();
    let tools = create_coding_tools(dir.path(), None);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["read", "write", "edit", "bash"]);
    assert!(tools[2].prompt_guidelines[0].contains("Use edit for precise changes"));
}

#[test]
fn tool_definitions_expose_pi_style_prompt_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let def = create_edit_tool_definition(dir.path());
    assert!(def.prompt_snippet.starts_with("Make precise file edits"));
    assert_eq!(def.prompt_guidelines.len(), 4);
}

#[test]
fn read_tool_schema_defines_line_controls_as_integers() {
    let dir = tempfile::tempdir().unwrap();
    let def = create_read_tool_definition(dir.path());
    let props = &def.input_schema["properties"];
    assert_eq!(props["offset"]["type"], "integer");
    assert_eq!(props["limit"]["type"], "integer");
}

#[tokio::test]
async fn read_tool_reads_file_with_offset_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let tool = create_read_tool(dir.path());

    let result = run(
        &tool,
        serde_json::json!({"path": "notes.txt", "offset": 2, "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        result.text(),
        "two\n\n[2 more lines in file. Use offset=3 to continue.]"
    );
    let details = result.details.as_ref().unwrap();
    assert_eq!(
        details["path"],
        dir.path().join("notes.txt").display().to_string()
    );
    assert!(details["truncation"].is_object());
}

#[tokio::test]
async fn read_tool_treats_zero_offset_as_start_of_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let tool = create_read_tool(dir.path());

    let result = run(
        &tool,
        serde_json::json!({"path": "notes.txt", "offset": 0, "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        result.text(),
        "one\n\n[3 more lines in file. Use offset=2 to continue.]"
    );
}

#[tokio::test]
async fn write_tool_creates_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let tool = create_write_tool(dir.path());

    let result = run(
        &tool,
        serde_json::json!({"path": "nested/file.txt", "content": "hello"}),
    )
    .await
    .unwrap();
    assert!(!result.text().is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nested/file.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn edit_tool_applies_multiple_exact_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    let tool = create_edit_tool(dir.path());

    let result = run(
        &tool,
        serde_json::json!({
            "path": "file.txt",
            "edits": [
                {"oldText": "alpha", "newText": "one"},
                {"oldText": "gamma", "newText": "three"},
            ],
        }),
    )
    .await
    .unwrap();
    assert!(!result.text().is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one\nbeta\nthree\n"
    );
}

#[tokio::test]
async fn edit_tool_rolls_back_when_any_edit_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let original = "alpha\nbeta\ngamma\n";
    std::fs::write(&path, original).unwrap();
    let tool = create_edit_tool(dir.path());

    let err = run_signal(
        &tool,
        serde_json::json!({
            "path": "file.txt",
            "edits": [
                {"oldText": "alpha", "newText": "one"},
                {"oldText": "missing", "newText": "nope"},
            ],
        }),
        None,
    )
    .await
    .unwrap_err();
    assert!(err.0.contains("Could not find edits[1]"), "{}", err.0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[tokio::test]
async fn edit_tool_requires_unique_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "repeat\nrepeat\n").unwrap();
    let tool = create_edit_tool(dir.path());

    let err = run(
        &tool,
        serde_json::json!({"path": "file.txt", "edits": [{"oldText": "repeat", "newText": "once"}]}),
    )
    .await
    .unwrap_err();
    assert!(err.0.contains("Found 2 occurrences"), "{}", err.0);
}

#[tokio::test]
async fn bash_tool_captures_stdout_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let tool = create_bash_tool(dir.path(), None);

    let result = run(&tool, serde_json::json!({"command": "printf hello"}))
        .await
        .unwrap();
    assert_eq!(result.text(), "hello");
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["exit_code"], 0);
    assert_eq!(details["timed_out"], false);
}

#[tokio::test]
async fn create_coding_tools_applies_shell_command_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let tools = create_coding_tools(
        dir.path(),
        Some("shopt -s expand_aliases\nalias greet='printf coding-tool-alias'"),
    );
    let bash = tools.iter().find(|t| t.name == "bash").unwrap();

    let result = run(bash, serde_json::json!({"command": "greet"}))
        .await
        .unwrap();
    assert_eq!(result.text(), "coding-tool-alias");
    assert_eq!(
        result.details.as_ref().unwrap()["shell_command_prefix_applied"],
        true
    );
}

#[tokio::test]
async fn bash_tool_reports_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let tool = create_bash_tool(dir.path(), None);

    let result = run(
        &tool,
        serde_json::json!({"command": "sleep 1", "timeout": 0.01}),
    )
    .await
    .unwrap();
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["timed_out"], true);
    assert!(result.text().contains("timed out"));
}

#[tokio::test]
async fn bash_tool_timeout_kills_shell_children() {
    let dir = tempfile::tempdir().unwrap();
    let tool = create_bash_tool(dir.path(), None);
    let marker = dir.path().join("marker");

    let start = Instant::now();
    let result = run(
        &tool,
        serde_json::json!({"command": "(sleep 0.25; touch marker) & wait", "timeout": 0.01}),
    )
    .await
    .unwrap();
    let duration = start.elapsed();
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;

    assert_eq!(result.details.as_ref().unwrap()["timed_out"], true);
    assert!(duration.as_secs_f64() < 0.5);
    assert!(
        !marker.exists(),
        "shell child survived the process-group kill"
    );
}

#[tokio::test]
async fn bash_tool_timeout_fires_when_streams_close_before_exit() {
    // Regression: a command that redirects/closes both output streams reaches
    // pipe EOF long before it exits. The timeout must still fire against the
    // process lifetime, not hang until the sleep finishes.
    let dir = tempfile::tempdir().unwrap();
    let tool = create_bash_tool(dir.path(), None);

    let start = Instant::now();
    let result = run(
        &tool,
        serde_json::json!({"command": "exec >/dev/null 2>&1; sleep 5", "timeout": 0.2}),
    )
    .await
    .unwrap();
    let duration = start.elapsed();

    assert_eq!(result.details.as_ref().unwrap()["timed_out"], true);
    assert!(
        duration.as_secs_f64() < 2.0,
        "timeout did not fire promptly ({duration:?})"
    );
}

#[tokio::test]
async fn bash_tool_timeout_fires_when_backgrounded_child_holds_pipe() {
    // Regression (team-lead HIGH): a backgrounded fd-inheriting child keeps the
    // output pipe open after the shell exits at t≈0. The timeout must still fire
    // against the *whole* communicate (exit + drain), not hang on the drain.
    let dir = tempfile::tempdir().unwrap();
    let tool = create_bash_tool(dir.path(), None);

    let start = Instant::now();
    let result = run(
        &tool,
        serde_json::json!({"command": "sleep 30 &", "timeout": 1.0}),
    )
    .await
    .unwrap();
    let duration = start.elapsed();

    assert_eq!(result.details.as_ref().unwrap()["timed_out"], true);
    assert!(
        duration.as_secs_f64() < 5.0,
        "timeout did not fire; drain hung ({duration:?})"
    );
}

#[tokio::test]
async fn bash_tool_cancellation_kills_shell_children() {
    let dir = tempfile::tempdir().unwrap();
    let tool = Arc::new(create_bash_tool(dir.path(), None));
    let token = Arc::new(SimpleCancellationToken::new());

    let task = {
        let tool = tool.clone();
        let token = token.clone();
        tokio::spawn(async move {
            run_signal(
                &tool,
                serde_json::json!({"command": "sleep 1 & wait"}),
                Some(token as Arc<dyn CancellationToken>),
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    token.cancel();
    let start = Instant::now();
    let result = task.await.unwrap().unwrap();
    let duration = start.elapsed();

    assert_eq!(result.details.as_ref().unwrap()["cancelled"], true);
    assert!(result.text().contains("cancelled"));
    assert!(duration.as_secs_f64() < 0.5);
}
