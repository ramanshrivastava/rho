//! Tests for the slash-command registry (port of tau `tests/test_commands.py`).
//!
//! Skipped tau cases: none. Every case in `test_commands.py` is ported. The
//! `available_model_choices` / `set_provider` / `tui_theme` fields on tau's
//! `FakeSession` are unused by the registry (they exist for TUI-only paths) and
//! are omitted here.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use rho_agent::tools::AgentTool;

use crate::paths::RhoPaths;
use crate::prompt_templates::PromptTemplate;
use crate::resources::ResourceDiagnostic;
use crate::session_manager::SessionManager;
use crate::skills::Skill;
use crate::system_prompt::ProjectContextFile;
use crate::tools::create_coding_tools;

use super::{CommandRegistry, CommandSession, SlashCommand, create_default_command_registry};

struct FakeSession {
    cwd: PathBuf,
    provider_name: String,
    model: String,
    available_models: Vec<String>,
    available_providers: Vec<String>,
    tools: Vec<AgentTool>,
    skills: Vec<Skill>,
    prompt_templates: Vec<PromptTemplate>,
    context_files: Vec<ProjectContextFile>,
    context_token_estimate: i64,
    auto_compact_token_threshold: Option<i64>,
    context_window_tokens: i64,
    thinking_level: String,
    available_thinking_levels: Vec<String>,
    thinking_unavailable_reason: Option<String>,
    resource_diagnostics: Vec<ResourceDiagnostic>,
    system_prompt: String,
    session_id: Option<String>,
    session_title: Option<String>,
    session_manager: Option<SessionManager>,
    ensure_session_indexed_called: bool,
    provider_reload_called: bool,
    reload_provider_settings_error: Option<String>,
}

impl FakeSession {
    fn new(cwd: &Path, manager: Option<SessionManager>) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            provider_name: "openai".to_string(),
            model: "fake-model".to_string(),
            available_models: vec!["fake-model".to_string(), "other-model".to_string()],
            available_providers: vec!["openai".to_string(), "local".to_string()],
            tools: create_coding_tools(cwd, None),
            skills: vec![Skill {
                name: "review".to_string(),
                path: cwd.join("review.md"),
                content: "Review code".to_string(),
                description: Some("Review code".to_string()),
            }],
            prompt_templates: Vec::new(),
            context_files: vec![ProjectContextFile {
                path: cwd.join("AGENTS.md").to_string_lossy().into_owned(),
                content: "Follow instructions.".to_string(),
            }],
            context_token_estimate: 123,
            auto_compact_token_threshold: Some(200),
            context_window_tokens: 584,
            thinking_level: "medium".to_string(),
            available_thinking_levels: ["off", "minimal", "low", "medium", "high", "xhigh"]
                .iter()
                .map(|level| (*level).to_string())
                .collect(),
            thinking_unavailable_reason: None,
            resource_diagnostics: Vec::new(),
            system_prompt: "You are Tau.\nFollow project instructions.".to_string(),
            session_id: Some("session-1".to_string()),
            session_title: None,
            session_manager: manager,
            ensure_session_indexed_called: false,
            provider_reload_called: false,
            reload_provider_settings_error: None,
        }
    }
}

impl CommandSession for FakeSession {
    fn cwd(&self) -> &Path {
        &self.cwd
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn provider_name(&self) -> &str {
        &self.provider_name
    }
    fn available_models(&self) -> Vec<String> {
        self.available_models.clone()
    }
    fn available_providers(&self) -> Vec<String> {
        self.available_providers.clone()
    }
    fn tools_len(&self) -> usize {
        self.tools.len()
    }
    fn skills(&self) -> &[Skill] {
        &self.skills
    }
    fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.prompt_templates
    }
    fn context_files(&self) -> &[ProjectContextFile] {
        &self.context_files
    }
    fn context_token_estimate(&self) -> i64 {
        self.context_token_estimate
    }
    fn auto_compact_token_threshold(&self) -> Option<i64> {
        self.auto_compact_token_threshold
    }
    fn context_window_tokens(&self) -> i64 {
        self.context_window_tokens
    }
    fn thinking_level(&self) -> &str {
        &self.thinking_level
    }
    fn available_thinking_levels(&self) -> Vec<String> {
        self.available_thinking_levels.clone()
    }
    fn resource_diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.resource_diagnostics
    }
    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn session_title(&self) -> Option<&str> {
        self.session_title.as_deref()
    }
    fn session_manager(&self) -> Option<&SessionManager> {
        self.session_manager.as_ref()
    }
    fn thinking_unavailable_reason(&self) -> Option<String> {
        self.thinking_unavailable_reason.clone()
    }

    fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }
    fn reload_provider_settings(&mut self) -> Result<(), String> {
        if let Some(error) = &self.reload_provider_settings_error {
            return Err(error.clone());
        }
        self.provider_reload_called = true;
        Ok(())
    }
    fn ensure_session_indexed(&mut self) {
        self.ensure_session_indexed_called = true;
        if let Some(manager) = &self.session_manager {
            let _ = manager.create_session(
                &self.cwd,
                &self.model,
                Some(&self.provider_name),
                None,
                self.session_id.as_deref(),
            );
        }
    }
}

fn manager_for(tmp: &Path) -> SessionManager {
    SessionManager::new(RhoPaths::new(tmp.join(".rho"), tmp.join(".agents")))
}

fn tmp() -> TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn registry_ignores_ordinary_prompts_and_skill_expansion() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    assert!(!registry.execute(&mut session, "hello").handled);
    assert!(
        !registry
            .execute(&mut session, "/skill:review fix this")
            .handled
    );
}

#[test]
fn registry_ignores_unregistered_slash_prompts() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    for prompt in ["/missing", "/README.md", "/tmp", "/Users/me/screenshot.png"] {
        let result = registry.execute(&mut session, prompt);
        assert!(!result.handled);
        assert!(result.message.is_none());
    }
}

#[test]
fn registered_commands_are_pi_aligned() {
    let registry = create_default_command_registry();
    let names: Vec<&str> = registry
        .list_commands()
        .iter()
        .map(|command| command.name.as_str())
        .collect();

    assert_eq!(
        names,
        [
            "compact",
            "export",
            "hotkeys",
            "login",
            "logout",
            "model",
            "name",
            "new",
            "quit",
            "reload",
            "resume",
            "scoped-models",
            "session",
            "skill",
            "system",
            "theme",
            "tree",
        ]
    );
}

#[test]
fn system_command_returns_active_prompt() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/system");
    assert!(result.handled);
    assert_eq!(
        result.message.as_deref(),
        Some("You are Tau.\nFollow project instructions.")
    );
    assert_eq!(
        registry
            .execute(&mut session, "/system extra")
            .message
            .as_deref(),
        Some("Usage: /system")
    );
}

#[test]
fn quit_and_new_return_control_flags() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    assert!(registry.execute(&mut session, "/quit").exit_requested);
    assert!(registry.execute(&mut session, "/exit").exit_requested);
    assert!(!registry.execute(&mut session, "/q").handled);
    assert!(registry.execute(&mut session, "/new").new_session_requested);
    assert!(!registry.execute(&mut session, "/clear").handled);
}

#[test]
fn compact_command_accepts_optional_instructions() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let default = registry.execute(&mut session, "/compact");
    let requested = registry.execute(&mut session, "/compact Summary of prior work.");

    assert_eq!(default.compact_summary.as_deref(), Some(""));
    assert_eq!(
        requested.compact_summary.as_deref(),
        Some("Summary of prior work.")
    );
}

#[test]
fn tree_command_requests_picker() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/tree");
    let with_args = registry.execute(&mut session, "/tree root");

    assert!(result.handled);
    assert!(result.tree_picker_requested);
    assert_eq!(with_args.message.as_deref(), Some("Usage: /tree"));
}

#[test]
fn export_command_requests_default_export() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/export");
    assert!(result.handled);
    assert!(result.export_requested);
    assert!(result.export_destination.is_none());
    assert!(result.export_format.is_none());
}

#[test]
fn export_command_parses_format_and_destination() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/export --format jsonl exports/session.jsonl");
    assert!(result.export_requested);
    assert_eq!(result.export_format.as_deref(), Some("jsonl"));
    assert_eq!(
        result.export_destination,
        Some(PathBuf::from("exports/session.jsonl"))
    );
}

#[test]
fn session_command_includes_session_details() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/session");
    let message = result.message.expect("message");
    assert!(message.contains("Model: fake-model"));
    assert!(message.contains(&format!("CWD: {}", dir.path().display())));
    assert!(message.contains("Tools: 4"));
    assert!(message.contains("Skills: 1"));
    assert!(message.contains("Context files: 1"));
    assert!(message.contains("Estimated context tokens: 123"));
    assert!(message.contains("Context window: 584"));
    assert!(message.contains("Thinking mode: medium"));
    assert!(message.contains("Auto compact threshold: 200"));
    assert!(message.contains("Resource diagnostics: 0"));
    assert!(message.contains("Session: session-1"));
    assert!(!message.contains("Session name:"));

    assert!(!registry.execute(&mut session, "/status").handled);
}

#[test]
fn session_command_includes_named_session_title() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);
    session.session_title = Some("Customer bugfix".to_string());

    let result = registry.execute(&mut session, "/session");
    let message = result.message.expect("message");
    assert!(message.contains("Session: session-1"));
    assert!(message.contains("Session name: Customer bugfix"));
}

#[test]
fn session_command_explains_unavailable_thinking_controls() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);
    session.available_thinking_levels = Vec::new();
    session.thinking_unavailable_reason =
        Some("Provider local does not declare thinking_levels".to_string());

    let result = registry.execute(&mut session, "/session");
    let message = result.message.expect("message");
    assert!(message.contains("Thinking mode: unavailable"));
    assert!(
        message.contains("Thinking unavailable: Provider local does not declare thinking_levels")
    );
    assert!(!message.contains("Thinking mode: medium"));
}

#[test]
fn hotkeys_command_lists_common_tui_shortcuts() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/hotkeys");
    let message = result.message.expect("message");
    assert!(message.contains("Common keyboard shortcuts:"));
    assert!(message.contains("Ctrl+K: open slash-command completions"));
    assert!(message.contains("Ctrl+R: open session picker"));
    assert!(message.contains("Shift+Tab: cycle thinking mode"));
}

#[test]
fn model_command_requests_picker_and_switches_models() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let list_result = registry.execute(&mut session, "/model");
    let switch_result = registry.execute(&mut session, "/model other-model");

    assert!(list_result.model_picker_requested);
    assert_eq!(
        switch_result.message.as_deref(),
        Some("Current model: other-model")
    );
    assert_eq!(session.model, "other-model");
    assert!(session.provider_reload_called);
}

#[test]
fn scoped_models_command_requests_scoped_picker() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let dashed_result = registry.execute(&mut session, "/scoped-models");
    let pi_style_result = registry.execute(&mut session, "/scoped models");

    assert!(dashed_result.scoped_models_picker_requested);
    assert!(pi_style_result.scoped_models_picker_requested);
    assert!(session.provider_reload_called);
}

#[test]
fn model_command_rejects_unknown_model() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/model missing");
    let message = result.message.expect("message");
    assert!(message.contains("Unknown model for provider openai: missing"));
    assert_eq!(session.model, "fake-model");
}

#[test]
fn model_command_reports_provider_refresh_failure() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);
    session.reload_provider_settings_error = Some("providers.json is invalid".to_string());

    let result = registry.execute(&mut session, "/model");
    assert_eq!(
        result.message.as_deref(),
        Some("Could not refresh provider settings: providers.json is invalid")
    );
    assert!(!result.model_picker_requested);
}

#[test]
fn theme_command_requests_picker_and_sets_theme() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let list_result = registry.execute(&mut session, "/theme");
    let switch_result = registry.execute(&mut session, "/theme tau-light");
    let unknown_result = registry.execute(&mut session, "/theme solarized");

    assert!(list_result.theme_picker_requested);
    assert_eq!(switch_result.theme.as_deref(), Some("tau-light"));
    let message = unknown_result.message.expect("message");
    assert!(message.contains("Unknown theme: solarized"));
}

#[test]
fn non_pi_commands_are_not_registered() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    for command in ["/provider", "/skills", "/resources", "/context", "/help"] {
        let result = registry.execute(&mut session, command);
        assert!(!result.handled);
        assert!(result.message.is_none());
    }
}

#[test]
fn login_command_requests_provider_picker() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/login");
    assert!(result.handled);
    assert!(result.login_picker_requested);
}

#[test]
fn login_command_requests_provider_login() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/login openai");
    assert!(result.handled);
    assert_eq!(result.login_provider.as_deref(), Some("openai"));
}

#[test]
fn login_command_resolves_anthropic_auth_aliases() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let api_result = registry.execute(&mut session, "/login anthropic-api");
    let subscription_result = registry.execute(&mut session, "/login anthropic-subscription");

    assert_eq!(api_result.login_provider.as_deref(), Some("anthropic"));
    assert_eq!(api_result.login_method.as_deref(), Some("api-key"));
    assert_eq!(
        subscription_result.login_provider.as_deref(),
        Some("anthropic")
    );
    assert_eq!(
        subscription_result.login_method.as_deref(),
        Some("subscription")
    );
}

#[test]
fn login_command_lists_auth_aliases_for_unknown_provider() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/login missing");
    let message = result.message.expect("message");
    assert!(message.contains("anthropic-api"));
    assert!(message.contains("anthropic-subscription"));
}

#[test]
fn login_command_requests_custom_provider_login() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/login custom");
    assert!(result.handled);
    assert!(result.custom_provider_login_requested);
}

#[test]
fn logout_command_requests_provider_picker() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/logout");
    assert!(result.handled);
    assert!(result.logout_picker_requested);
}

#[test]
fn logout_command_requests_provider_logout() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/logout openai");
    assert!(result.handled);
    assert_eq!(result.logout_provider.as_deref(), Some("openai"));
}

#[test]
fn logout_command_rejects_unknown_provider() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/logout local");
    assert!(result.handled);
    let message = result.message.expect("message");
    assert!(message.contains("Unknown logout provider: local"));
}

#[test]
fn reload_command_requests_async_session_reload() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/reload");
    assert!(result.handled);
    assert!(result.reload_requested);
    assert!(result.message.is_none());
    assert!(!session.provider_reload_called);
}

#[test]
fn resume_without_argument_requests_picker() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let mut session = FakeSession::new(dir.path(), Some(manager));

    let result = registry.execute(&mut session, "/resume");
    assert!(result.resume_picker_requested);
    assert!(result.message.is_none());
    assert!(!registry.execute(&mut session, "/sessions").handled);
}

#[test]
fn resume_command_requests_indexed_session() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let record = manager.create_session(dir.path(), "fake-model", None, Some("Test session"), None);
    let record_id = record.id.clone();
    let mut session = FakeSession::new(dir.path(), Some(manager));

    let result = registry.execute(&mut session, &format!("/resume {record_id}"));
    assert_eq!(
        result.resume_session_id.as_deref(),
        Some(record_id.as_str())
    );
    assert!(result.message.is_none());
}

#[test]
fn resume_command_rejects_missing_or_unknown_session() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let mut session = FakeSession::new(dir.path(), Some(manager));

    let unknown = registry.execute(&mut session, "/resume missing");
    assert_eq!(unknown.message.as_deref(), Some("Unknown session: missing"));
}

#[test]
fn name_command_shows_current_name_and_usage() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let record = manager.create_session(dir.path(), "fake-model", None, Some("Test session"), None);
    let mut session = FakeSession::new(dir.path(), Some(manager));
    session.session_id = Some(record.id.clone());

    let result = registry.execute(&mut session, "/name");
    assert_eq!(
        result.message.as_deref(),
        Some("Current session name: Test session\nUsage: /name <new name>")
    );
}

#[test]
fn name_command_renames_current_session() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let record = manager.create_session(dir.path(), "fake-model", None, Some("Old name"), None);
    let record_id = record.id.clone();
    let created_at = record.updated_at;
    let verify_manager = manager_for(dir.path());
    let mut session = FakeSession::new(dir.path(), Some(manager));
    session.session_id = Some(record_id.clone());

    let result = registry.execute(&mut session, "/name Customer bugfix");
    assert_eq!(
        result.message.as_deref(),
        Some("Session renamed: Customer bugfix")
    );

    let renamed = verify_manager
        .get_session(&record_id)
        .expect("renamed record");
    assert_eq!(renamed.title.as_deref(), Some("Customer bugfix"));
    assert_eq!(renamed.model, "fake-model");
    assert!(renamed.updated_at >= created_at);
}

#[test]
fn name_command_indexes_pending_session_before_renaming() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let verify_manager = manager_for(dir.path());
    let mut session = FakeSession::new(dir.path(), Some(manager));
    session.session_id = Some("pending-session".to_string());

    let result = registry.execute(&mut session, "/name Customer bugfix");
    assert_eq!(
        result.message.as_deref(),
        Some("Session renamed: Customer bugfix")
    );
    assert!(session.ensure_session_indexed_called);

    let record = verify_manager
        .get_session("pending-session")
        .expect("indexed record");
    assert_eq!(record.title.as_deref(), Some("Customer bugfix"));
}

#[test]
fn name_command_reports_missing_session_manager() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let mut session = FakeSession::new(dir.path(), None);

    let result = registry.execute(&mut session, "/name Work");
    assert_eq!(
        result.message.as_deref(),
        Some("Session manager is not available.")
    );
}

#[test]
fn name_command_rejects_multiline_name() {
    let registry = create_default_command_registry();
    let dir = tmp();
    let manager = manager_for(dir.path());
    let record = manager.create_session(dir.path(), "fake-model", None, None, None);
    let record_id = record.id.clone();
    let verify_manager = manager_for(dir.path());
    let mut session = FakeSession::new(dir.path(), Some(manager));
    session.session_id = Some(record_id.clone());

    let result = registry.execute(&mut session, "/name Bad\nName");
    assert_eq!(
        result.message.as_deref(),
        Some("Session name must be a single line.")
    );
    let unchanged = verify_manager.get_session(&record_id).expect("record");
    assert_eq!(unchanged.title, record.title);
}

#[test]
fn registry_rejects_duplicate_commands_and_aliases() {
    let mut registry = CommandRegistry::new();
    let command = SlashCommand::new("test", "/test", "Test", |context| {
        create_default_command_registry().execute(context.session, "/session")
    });
    registry
        .register(command.clone())
        .expect("first registration");

    let error = registry
        .register(command)
        .expect_err("duplicate command should fail");
    assert!(error.contains("Duplicate slash command"));
}
