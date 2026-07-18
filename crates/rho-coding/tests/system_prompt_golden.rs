//! Golden test: rho's assembled system prompt for the default coding-tool set
//! must be byte-identical to tau's, pinned by
//! `fixtures/system-prompt/default_coding_tools.txt` (extracted by
//! `tools/extract-fixtures/extract_system_prompt.py`, same fixed cwd + date).
//!
//! If this diffs, rho's `build_system_prompt` is wrong — never edit the fixture
//! (see AGENTS.md fixture policy).

use std::path::PathBuf;

use rho_coding::{BuildSystemPromptOptions, Date, build_system_prompt, create_coding_tools};

const GOLDEN: &str = include_str!("../../../fixtures/system-prompt/default_coding_tools.txt");

#[test]
fn default_coding_tools_prompt_matches_tau_golden() {
    let cwd = PathBuf::from("/tmp/rho-fixture-cwd");
    let tools = create_coding_tools(&cwd, None);
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        cwd,
        tools,
        current_date: Some(Date::new(2026, 6, 17)),
        // The sanctioned identity seam: parity mode assembles the prompt with
        // brand = "Tau" so it stays byte-identical to tau. The production
        // default is "rho" (see `system_prompt::BuildSystemPromptOptions::brand`
        // and `dev-notes/identity-vs-parity.md`). Never edit the fixture.
        brand: Some("Tau".into()),
        ..Default::default()
    });

    assert_eq!(prompt, GOLDEN, "system prompt diverged from tau golden");
}
