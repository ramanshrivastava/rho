//! Binary-level regression test for the credential-less TUI launch path.
//!
//! PR #10 fixed `build_interactive_session` so that a credential-less `rho`
//! substitutes a `LoginRequiredProvider` placeholder instead of aborting — but
//! only its *unit* tests exercised `build_interactive_session` directly. The
//! real binary still hard-errored: `CodingSession::load` eagerly calls
//! `refresh_runtime_provider`, which rebuilt the real (credential-less) provider
//! from the runtime config the placeholder path had wrongly kept, hitting
//! "Missing provider API key" and exiting before the TUI ever rendered.
//!
//! These tests run the ACTUAL compiled binary with a cleared environment and an
//! empty `RHO_HOME`, closing the unit-vs-binary gap. The launch has no PTY, so
//! ratatui's terminal init fails immediately with an OS "device not configured"
//! error — which is exactly the point: reaching TUI init proves we got *past*
//! the credential resolution that used to abort. The assertion is therefore
//! "the missing-key error is NOT emitted" (tau parity), not that the TUI runs.

use std::path::Path;
use std::process::{Command, Stdio};

/// Substring of the tau-parity error that must NOT appear once the fix is in.
const MISSING_KEY_ERROR: &str = "Missing provider API key";

/// Run the compiled `rho` binary with a hermetic, credential-free environment.
///
/// `rho_home` becomes both `RHO_HOME` and `HOME`, so no real user credentials or
/// session records leak in. stdin is `/dev/null` (no PTY) so the TUI init aborts
/// promptly instead of blocking on input.
fn run_rho(rho_home: &Path, extra_args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rho"));
    cmd.env_clear()
        .env("HOME", rho_home)
        .env("RHO_HOME", rho_home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("TERM", "xterm")
        .args(extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.output().expect("failed to spawn rho binary")
}

/// Fresh launch, empty `RHO_HOME`, no credentials: must reach TUI init rather
/// than aborting with the missing-key error.
#[test]
fn credentialless_fresh_launch_does_not_error_on_missing_key() {
    let home = tempfile::tempdir().expect("create temp RHO_HOME");
    let output = run_rho(home.path(), &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains(MISSING_KEY_ERROR),
        "credential-less fresh launch regressed: binary aborted with the \
         missing-key error instead of launching the login-required TUI.\n\
         stderr:\n{stderr}"
    );
}

/// Resume of a session whose stored provider has no credentials: tau substitutes
/// the placeholder here too. Must not abort with the missing-key error.
#[test]
fn credentialless_resume_launch_does_not_error_on_missing_key() {
    use rho_coding::paths::RhoPaths;
    use rho_coding::session_manager::SessionManager;

    let home = tempfile::tempdir().expect("create temp RHO_HOME");
    let agents = tempfile::tempdir().expect("create temp agents home");

    // Create + index a resumable session whose stored provider is a real,
    // credentialed-in-production provider (openai) — so the resume path resolves
    // that provider, finds no usable credential, and must fall through to the
    // login-required placeholder rather than erroring.
    let paths = RhoPaths::new(home.path().to_path_buf(), agents.path().to_path_buf());
    let manager = SessionManager::new(paths);
    let cwd = home.path().join("project");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    let record = manager.create_session(&cwd, "gpt-5.4", Some("openai"), None, None);

    let output = run_rho(home.path(), &["--resume", &record.id]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains(MISSING_KEY_ERROR),
        "credential-less resume regressed: binary aborted with the missing-key \
         error instead of launching the login-required TUI.\nstderr:\n{stderr}"
    );
}
