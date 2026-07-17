//! Port of tau `tests/test_tui_autocomplete.py` (logic assertions).
//!
//! The two rendering-only cases (`render_completion_suggestions`) live in the
//! widget snapshot suite; every trigger/selection assertion is ported here
//! against the real command registry.

use std::path::PathBuf;

use rho_coding::commands::create_default_command_registry;
use rho_coding::prompt_templates::PromptTemplate;
use rho_coding::skills::Skill;
use rho_tui::autocomplete::{CompletionInputs, CompletionOption, build_completion_state};

fn skill(name: &str, description: &str) -> Skill {
    Skill {
        name: name.into(),
        path: PathBuf::from(format!("{name}.md")),
        content: "Review code".into(),
        description: Some(description.into()),
    }
}

fn prompt(name: &str, description: Option<&str>) -> PromptTemplate {
    PromptTemplate {
        name: name.into(),
        path: PathBuf::from(format!("{name}.md")),
        content: "Example prompt.".into(),
        description: description.map(str::to_string),
    }
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_string()).collect()
}

fn displays(state: &rho_tui::CompletionState) -> Vec<String> {
    state.items.iter().map(|i| i.display.clone()).collect()
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rho-tui-ac-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn build(text: &str, inputs: &CompletionInputs) -> rho_tui::CompletionState {
    build_completion_state(text, &create_default_command_registry(), inputs)
}

#[test]
fn slash_lists_every_registered_command() {
    let registry = create_default_command_registry();
    let state = build_completion_state("/", &registry, &CompletionInputs::default());
    let expected: Vec<String> = registry
        .list_commands()
        .iter()
        .map(|c| {
            if c.name == "skill" {
                "/skill:".to_string()
            } else {
                format!("/{}", c.name)
            }
        })
        .collect();
    assert_eq!(displays(&state), expected);
}

#[test]
fn groups_commands_and_custom_prompts() {
    let templates = vec![prompt("example", Some("Run example."))];
    let inputs = CompletionInputs {
        prompt_templates: &templates,
        ..Default::default()
    };
    let state = build("/", &inputs);
    assert_eq!(state.items[0].category.as_deref(), Some("Commands"));
    assert_eq!(state.items.last().unwrap().display, "/example");
    assert_eq!(
        state.items.last().unwrap().category.as_deref(),
        Some("Custom prompts")
    );
}

#[test]
fn suggests_registered_commands() {
    let state = build("/se", &CompletionInputs::default());
    assert_eq!(displays(&state), vec!["/session"]);
    assert_eq!(state.selected().unwrap().apply("/se"), "/session");
}

#[test]
fn matches_search_terms_with_canonical_replacement() {
    let clear = build("/cl", &CompletionInputs::default());
    assert_eq!(displays(&clear), vec!["/new"]);
    assert_eq!(clear.selected().unwrap().apply("/cl"), "/new");

    let sessions = build("/sess", &CompletionInputs::default());
    assert_eq!(displays(&sessions), vec!["/session"]);
    assert_eq!(sessions.selected().unwrap().apply("/sess"), "/session");
}

#[test]
fn prioritizes_direct_matches_over_search_terms() {
    let state = build("/res", &CompletionInputs::default());
    assert_eq!(displays(&state)[..2], ["/resume".to_string(), "/new".to_string()]);
    assert_eq!(state.selected().unwrap().apply("/res"), "/resume");
}

#[test]
fn skill_command_available_for_command_completion() {
    let state = build("/ski", &CompletionInputs::default());
    assert_eq!(displays(&state), vec!["/skill:"]);
    assert_eq!(state.selected().unwrap().apply("/ski"), "/skill:");
}

#[test]
fn skill_name_completion_preserves_request_text() {
    let skills = vec![skill("review", "Review code")];
    let inputs = CompletionInputs {
        skills: &skills,
        ..Default::default()
    };
    let state = build("/skill:r fix tests", &inputs);
    assert_eq!(displays(&state), vec!["/skill:review"]);
    assert_eq!(
        state.selected().unwrap().apply("/skill:r fix tests"),
        "/skill:review fix tests"
    );
}

#[test]
fn skill_name_completion_hides_after_completed_command_space() {
    let skills = vec![skill("review", "Review code")];
    let inputs = CompletionInputs {
        skills: &skills,
        ..Default::default()
    };
    assert!(build("/skill:review ", &inputs).items.is_empty());
    assert!(build("/skill:review fix tests", &inputs).items.is_empty());
}

#[test]
fn custom_prompt_hides_after_completed_command_space() {
    let templates = vec![prompt("example", Some("Run example."))];
    let inputs = CompletionInputs {
        prompt_templates: &templates,
        ..Default::default()
    };
    assert!(build("/example ", &inputs).items.is_empty());
    assert!(build("/example fix tests", &inputs).items.is_empty());
}

#[test]
fn builtin_command_hides_after_completed_command_space() {
    assert!(build("/compact ", &CompletionInputs::default()).items.is_empty());
    assert!(
        build("/compact summarize old context", &CompletionInputs::default())
            .items
            .is_empty()
    );
}

#[test]
fn argument_completion_wins_over_completed_command_hide() {
    let models = owned(&["fake-model"]);
    let inputs = CompletionInputs {
        model_names: &models,
        ..Default::default()
    };
    assert_eq!(displays(&build("/model fak", &inputs)), vec!["fake-model"]);
}

#[test]
fn argument_completion_wins_over_custom_prompt_name() {
    let models = owned(&["fake-model"]);
    let templates = vec![prompt("model", None)];
    let inputs = CompletionInputs {
        model_names: &models,
        prompt_templates: &templates,
        ..Default::default()
    };
    assert_eq!(displays(&build("/model fak", &inputs)), vec!["fake-model"]);
}

#[test]
fn custom_prompt_reappears_when_deleting_back_to_command_token() {
    let templates = vec![prompt("example", Some("Run example."))];
    let inputs = CompletionInputs {
        prompt_templates: &templates,
        ..Default::default()
    };
    assert_eq!(displays(&build("/exa", &inputs)), vec!["/example"]);
}

#[test]
fn selection_wraps() {
    let state = build("/s", &CompletionInputs::default());
    assert!(state.items.len() > 1);
    assert_eq!(state.select_previous().selected_index, state.items.len() - 1);
    assert_eq!(state.select_next().selected_index, 1);
}

#[test]
fn model_argument_preserves_existing_text() {
    let models = owned(&["fake-model", "other-model"]);
    let inputs = CompletionInputs {
        model_names: &models,
        ..Default::default()
    };
    let state = build("/model fak continue", &inputs);
    assert_eq!(displays(&state), vec!["fake-model"]);
    assert_eq!(
        state.selected().unwrap().apply("/model fak continue"),
        "/model fake-model continue"
    );
}

#[test]
fn provider_argument_completion_is_not_available() {
    let providers = owned(&["openai", "local"]);
    let inputs = CompletionInputs {
        provider_names: &providers,
        ..Default::default()
    };
    assert!(build("/provider lo", &inputs).items.is_empty());
}

#[test]
fn login_argument_uses_available_providers() {
    let providers = owned(&["openai", "openrouter", "anthropic"]);
    let inputs = CompletionInputs {
        provider_names: &providers,
        ..Default::default()
    };
    assert_eq!(displays(&build("/login op", &inputs)), vec!["openai", "openrouter"]);
}

#[test]
fn login_argument_includes_anthropic_auth_aliases() {
    let providers = owned(&["anthropic", "anthropic-api", "anthropic-subscription"]);
    let inputs = CompletionInputs {
        provider_names: &providers,
        ..Default::default()
    };
    assert_eq!(
        displays(&build("/login anthropic-", &inputs)),
        vec!["anthropic-api", "anthropic-subscription"]
    );
}

#[test]
fn logout_argument_uses_available_providers() {
    let providers = owned(&["openai", "openrouter", "anthropic"]);
    let inputs = CompletionInputs {
        provider_names: &providers,
        ..Default::default()
    };
    assert_eq!(displays(&build("/logout op", &inputs)), vec!["openai", "openrouter"]);
}

#[test]
fn thinking_argument_completion_is_not_available() {
    let levels = owned(&["off", "minimal", "low", "medium", "high", "xhigh"]);
    let inputs = CompletionInputs {
        thinking_levels: &levels,
        ..Default::default()
    };
    assert!(build("/thinking h", &inputs).items.is_empty());
}

#[test]
fn theme_argument_uses_theme_names() {
    let themes = owned(&["tau-dark", "tau-light", "high-contrast"]);
    let inputs = CompletionInputs {
        theme_names: &themes,
        ..Default::default()
    };
    let state = build("/theme tau-", &inputs);
    assert_eq!(displays(&state), vec!["tau-dark", "tau-light"]);
    assert_eq!(state.selected().unwrap().apply("/theme tau-"), "/theme tau-dark");
}

#[test]
fn resume_argument_uses_session_ids() {
    let ids = owned(&["session-1", "other"]);
    let inputs = CompletionInputs {
        session_ids: &ids,
        ..Default::default()
    };
    let state = build("/resume sess", &inputs);
    assert_eq!(displays(&state), vec!["session-1"]);
    assert_eq!(state.selected().unwrap().apply("/resume sess"), "/resume session-1");
}

#[test]
fn resume_argument_uses_session_options_with_descriptions() {
    let options = vec![
        CompletionOption::new("session-2", Some("Newer - qwen - /repo".into())),
        CompletionOption::new("session-1", Some("Older - gpt - /repo".into())),
    ];
    let inputs = CompletionInputs {
        session_options: &options,
        ..Default::default()
    };
    let state = build("/resume sess", &inputs);
    assert_eq!(displays(&state), vec!["session-2", "session-1"]);
    assert_eq!(
        state
            .items
            .iter()
            .map(|i| i.description.clone())
            .collect::<Vec<_>>(),
        vec![
            Some("Newer - qwen - /repo".to_string()),
            Some("Older - gpt - /repo".to_string()),
        ]
    );
}

#[test]
fn file_reference_matches_workspace_files() {
    let dir = temp_dir("fileref");
    std::fs::write(dir.join("README.md"), "# Project\n").unwrap();
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/app.py"), "print('hi')\n").unwrap();
    std::fs::write(dir.join(".hidden"), "secret\n").unwrap();
    std::fs::create_dir(dir.join("node_modules")).unwrap();
    std::fs::write(dir.join("node_modules/ignored.js"), "").unwrap();

    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    let state = build("please read @app", &inputs);
    assert_eq!(displays(&state), vec!["@src/app.py"]);
    assert_eq!(
        state.selected().unwrap().apply("please read @app"),
        "please read @src/app.py"
    );
}

#[test]
fn file_reference_stays_off_for_slash_commands() {
    let dir = temp_dir("fileref-slash");
    std::fs::write(dir.join("README.md"), "# Project\n").unwrap();
    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    assert!(build("/help @read", &inputs).items.is_empty());
}

fn shell_dir(label: &str) -> PathBuf {
    let dir = temp_dir(label);
    std::fs::write(dir.join("README.md"), "# Project\n").unwrap();
    dir
}

#[test]
fn shell_path_preserves_bang_prefix() {
    let dir = shell_dir("shell-bang");
    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    let state = build("!cat READ", &inputs);
    assert_eq!(displays(&state), vec!["README.md"]);
    assert_eq!(state.selected().unwrap().apply("!cat READ"), "!cat README.md");
}

#[test]
fn shell_path_preserves_double_bang_prefix() {
    let dir = shell_dir("shell-dbang");
    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    let state = build("!!cat READ", &inputs);
    assert_eq!(displays(&state), vec!["README.md"]);
    assert_eq!(state.selected().unwrap().apply("!!cat READ"), "!!cat README.md");
}

#[test]
fn shell_path_matches_relative_paths() {
    let dir = temp_dir("shell-rel");
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.py"), "print('hi')\n").unwrap();
    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    let state = build("!cat src/ma", &inputs);
    assert_eq!(displays(&state), vec!["src/main.py"]);
    assert_eq!(state.selected().unwrap().apply("!cat src/ma"), "!cat src/main.py");
}

#[test]
fn shell_path_adds_trailing_slash_for_directories() {
    let dir = temp_dir("shell-dir");
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.py"), "print('hi')\n").unwrap();
    let inputs = CompletionInputs {
        cwd: Some(&dir),
        ..Default::default()
    };
    let directory_state = build("!cat sr", &inputs);
    assert_eq!(displays(&directory_state), vec!["src/"]);
    assert_eq!(directory_state.selected().unwrap().apply("!cat sr"), "!cat src/");

    let child_state = build("!cat src/", &inputs);
    assert_eq!(displays(&child_state), vec!["src/main.py"]);
}
