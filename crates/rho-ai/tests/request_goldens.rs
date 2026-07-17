//! Request-payload golden tests: each adapter's request builder must produce
//! JSON **byte-identical** to the recorded `fixtures/sse/<provider>/<case>.request.json`.
//!
//! The fixtures were captured from tau via `compact(payload)` (compact separators,
//! `ensure_ascii=False`), which is exactly `serde_json::to_string` over an
//! insertion-order-preserving map — so a diff here is a bug in the rho request
//! builder (key order, defaults, or shape), never in the fixture.

mod support;

use std::path::{Path, PathBuf};

use rho_ai::{AnthropicConfig, OpenAICodexConfig, OpenAICompatibleConfig};
use serde_json::Value;
use support::{bash_tool, codex_creds_resolver, read_tool, user};

fn fixture(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/sse")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("read fixture {}", path.display()))
        .trim_end_matches('\n')
        .to_string()
}

fn assert_payload(actual: &Value, rel: &str) {
    let got = serde_json::to_string(actual).expect("serialize payload");
    let want = fixture(rel);
    assert_eq!(got, want, "\n{rel} request payload BYTE MISMATCH");
}

// --- anthropic --------------------------------------------------------------

#[test]
fn anthropic_text_request() {
    let config = AnthropicConfig::new("k").with_base_url("https://api.anthropic.test/v1");
    let payload = rho_ai::anthropic::build_messages_payload(
        &config,
        "claude-x",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "anthropic/text.request.json");
}

#[test]
fn anthropic_thinking_request() {
    // The `thinking` case sends the same request (no thinking config); the
    // fixture must still match byte-for-byte.
    let config = AnthropicConfig::new("k").with_base_url("https://api.anthropic.test/v1");
    let payload = rho_ai::anthropic::build_messages_payload(
        &config,
        "claude-x",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "anthropic/thinking.request.json");
}

#[test]
fn anthropic_tool_calls_request() {
    let config = AnthropicConfig::new("k").with_base_url("https://api.anthropic.test/v1");
    let payload = rho_ai::anthropic::build_messages_payload(
        &config,
        "claude-x",
        "You are Tau.",
        &[user("run ls")],
        &[bash_tool()],
    );
    assert_payload(&payload, "anthropic/tool_calls.request.json");
}

// --- openai_compatible ------------------------------------------------------

fn openai_config() -> OpenAICompatibleConfig {
    OpenAICompatibleConfig::new("k").with_base_url("https://x.test/v1")
}

#[test]
fn openai_compatible_text_request() {
    let payload = rho_ai::openai_compatible::build_chat_payload(
        &openai_config(),
        "gpt-x",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "openai_compatible/text.request.json");
}

#[test]
fn openai_compatible_reasoning_request() {
    // Same body as `text` (no reasoning_effort configured in the extraction).
    let payload = rho_ai::openai_compatible::build_chat_payload(
        &openai_config(),
        "gpt-x",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "openai_compatible/reasoning.request.json");
}

#[test]
fn openai_compatible_error_request() {
    let payload = rho_ai::openai_compatible::build_chat_payload(
        &openai_config().with_max_retries(0),
        "gpt-x",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "openai_compatible/error.request.json");
}

#[test]
fn openai_compatible_tool_calls_request() {
    let payload = rho_ai::openai_compatible::build_chat_payload(
        &openai_config(),
        "gpt-x",
        "You are Tau.",
        &[user("run ls")],
        &[read_tool()],
    );
    assert_payload(&payload, "openai_compatible/tool_calls.request.json");
}

// --- google -----------------------------------------------------------------

fn google_config() -> OpenAICompatibleConfig {
    OpenAICompatibleConfig::new("k")
        .with_base_url("https://generativelanguage.googleapis.com/v1beta")
}

#[test]
fn google_text_request() {
    let payload = rho_ai::google::build_google_payload(
        &google_config(),
        "gemini-2.5-flash",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "google/text.request.json");
}

#[test]
fn google_tool_calls_request() {
    let payload = rho_ai::google::build_google_payload(
        &google_config(),
        "gemini-2.5-flash",
        "You are Tau.",
        &[user("run ls")],
        &[bash_tool()],
    );
    assert_payload(&payload, "google/tool_calls.request.json");
}

// --- mistral ----------------------------------------------------------------

fn mistral_config() -> OpenAICompatibleConfig {
    OpenAICompatibleConfig::new("k").with_base_url("https://api.mistral.test/v1")
}

#[test]
fn mistral_text_request() {
    let payload = rho_ai::mistral::build_mistral_payload(
        &mistral_config(),
        "mistral-large",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "mistral/text.request.json");
}

#[test]
fn mistral_tool_calls_request() {
    let payload = rho_ai::mistral::build_mistral_payload(
        &mistral_config(),
        "mistral-large",
        "You are Tau.",
        &[user("run ls")],
        &[read_tool()],
    );
    assert_payload(&payload, "mistral/tool_calls.request.json");
}

// --- openai_codex -----------------------------------------------------------

fn codex_config() -> OpenAICodexConfig {
    OpenAICodexConfig::new(codex_creds_resolver()).with_base_url("https://chatgpt.test/backend-api")
}

#[test]
fn openai_codex_text_request() {
    let payload = rho_ai::openai_codex::build_codex_payload(
        &codex_config(),
        "gpt-5.5",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "openai_codex/text.request.json");
}

#[test]
fn openai_codex_reasoning_request() {
    // Same body as `text` (no reasoning effort configured).
    let payload = rho_ai::openai_codex::build_codex_payload(
        &codex_config(),
        "gpt-5.5",
        "You are Tau.",
        &[user("Say hello")],
        &[],
    );
    assert_payload(&payload, "openai_codex/reasoning.request.json");
}

#[test]
fn openai_codex_tool_calls_request() {
    let payload = rho_ai::openai_codex::build_codex_payload(
        &codex_config(),
        "gpt-5.5",
        "You are Tau.",
        &[user("run ls")],
        &[read_tool()],
    );
    assert_payload(&payload, "openai_codex/tool_calls.request.json");
}
