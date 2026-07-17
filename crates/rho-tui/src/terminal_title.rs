//! Terminal window/tab title updates (port of tau `tau_coding/tui/terminal_title.py`).
//!
//! **Rebrand divergence (journaled in `dev-notes/phase-5.md`).** tau marks the
//! title with `τ` and gates on `TAU_TERMINAL_TITLE`; rho uses `ρ` and
//! `RHO_TERMINAL_TITLE`, matching the product identity and the existing
//! `RHO_HOME` / `RHO_FAKE` environment convention. The logic is otherwise a 1:1
//! port; the transferred `test_terminal_title.py` expectations are adjusted for
//! the rebrand.

use std::io::{IsTerminal, Write};

use crate::pystr;

/// Maximum terminal-title length (codepoints).
pub const MAX_TERMINAL_TITLE_LENGTH: usize = 120;
/// OSC string terminator (`BEL`).
pub const OSC_TERMINATOR: char = '\u{7}';
/// The rho title mark (tau uses `τ`; see the module rebrand note).
pub const RHO_TITLE_MARK: &str = "ρ";
/// Spinner frames shown while the agent is running.
pub const RUNNING_TITLE_FRAMES: [&str; 10] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Environment variable gating title emission (tau: `TAU_TERMINAL_TITLE`).
pub const TITLE_ENV_VAR: &str = "RHO_TERMINAL_TITLE";

fn is_control_char(ch: char) -> bool {
    let code = ch as u32;
    code <= 0x1f || (0x7f..=0x9f).contains(&code)
}

/// Whether rho should emit OSC title sequences, given an environment lookup and
/// whether the output stream is a TTY (tau `terminal_title_supported`).
pub fn terminal_title_supported<F>(get_env: F, is_tty: bool) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    let title_flag = get_env(TITLE_ENV_VAR).unwrap_or_default().to_lowercase();
    if matches!(title_flag.as_str(), "0" | "false" | "no" | "off") {
        return false;
    }
    if !is_tty {
        return false;
    }
    if get_env("TERM").unwrap_or_default() == "dumb" {
        return false;
    }
    let ci = get_env("CI").unwrap_or_default();
    !(!ci.is_empty() && title_flag != "1")
}

/// Whether the process's real stdout supports title sequences.
#[must_use]
pub fn terminal_title_supported_default() -> bool {
    terminal_title_supported(
        |key| std::env::var(key).ok(),
        std::io::stdout().is_terminal(),
    )
}

/// Strip OSC-breaking control bytes and cap terminal-title text (tau
/// `sanitize_terminal_title`).
#[must_use]
pub fn sanitize_terminal_title(value: Option<&str>) -> String {
    sanitize_terminal_title_with(value, MAX_TERMINAL_TITLE_LENGTH)
}

fn sanitize_terminal_title_with(value: Option<&str>, max_length: usize) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let sanitized: String = value.chars().filter(|c| !is_control_char(*c)).collect();
    let sanitized = sanitized.trim().to_string();
    if pystr::char_len(&sanitized) <= max_length {
        return sanitized;
    }
    if max_length <= 1 {
        return pystr::char_prefix(&sanitized, max_length).to_string();
    }
    format!(
        "{}…",
        pystr::char_prefix(&sanitized, max_length - 1).trim_end()
    )
}

/// Return rho's terminal tab title for the current session/running state (tau
/// `build_terminal_title`).
#[must_use]
pub fn build_terminal_title(session_title: Option<&str>, running: bool, frame: usize) -> String {
    let title = sanitize_terminal_title(session_title);
    let title = if title.is_empty() || title.to_lowercase() == "untitled session" {
        RHO_TITLE_MARK.to_string()
    } else {
        format!("{RHO_TITLE_MARK} | {title}")
    };
    if !running {
        return title;
    }
    let frame = RUNNING_TITLE_FRAMES[frame % RUNNING_TITLE_FRAMES.len()];
    format!("{frame} {title}")
}

/// Return an OSC 0 sequence that sets the terminal window/tab title (tau
/// `osc_terminal_title_sequence`).
#[must_use]
pub fn osc_terminal_title_sequence(title: &str) -> String {
    format!(
        "\x1b]0;{}{OSC_TERMINATOR}",
        sanitize_terminal_title(Some(title))
    )
}

type TitleWriter = Box<dyn FnMut(&str) -> std::io::Result<()> + Send>;

/// Small stateful writer that avoids duplicate OSC title writes (tau
/// `TerminalTitleController`).
pub struct TerminalTitleController {
    /// Whether title writes are currently emitted.
    pub enabled: bool,
    writer: TitleWriter,
    last_title: Option<String>,
    exit_title: String,
}

impl TerminalTitleController {
    /// Build a controller detecting support from the real stdout.
    #[must_use]
    pub fn new() -> Self {
        Self::with_writer(terminal_title_supported_default(), default_writer())
    }

    /// Build a controller with an explicit enabled flag and writer (for tests).
    pub fn with_writer(enabled: bool, writer: TitleWriter) -> Self {
        Self {
            enabled,
            writer,
            last_title: None,
            exit_title: RHO_TITLE_MARK.to_string(),
        }
    }

    /// Write the current rho title if it differs from the last emitted title.
    pub fn update(&mut self, session_title: Option<&str>, running: bool, frame: usize) {
        if !self.enabled {
            return;
        }
        let title = build_terminal_title(session_title, running, frame);
        if Some(&title) == self.last_title.as_ref() {
            return;
        }
        if self.write(&osc_terminal_title_sequence(&title)) {
            self.last_title = Some(title);
        }
    }

    /// Leave the terminal title in a neutral idle rho state on shutdown.
    pub fn restore(&mut self) {
        if !self.enabled {
            return;
        }
        let sequence = osc_terminal_title_sequence(&self.exit_title.clone());
        if self.write(&sequence) {
            self.last_title = Some(self.exit_title.clone());
        }
    }

    fn write(&mut self, sequence: &str) -> bool {
        match (self.writer)(sequence) {
            Ok(()) => true,
            Err(_) => {
                self.enabled = false;
                false
            }
        }
    }
}

impl Default for TerminalTitleController {
    fn default() -> Self {
        Self::new()
    }
}

fn default_writer() -> TitleWriter {
    Box::new(|sequence: &str| {
        let mut stdout = std::io::stdout();
        stdout.write_all(sequence.as_bytes())?;
        stdout.flush()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn build_terminal_title_uses_session_name_and_running_frame() {
        assert_eq!(build_terminal_title(Some("build notes"), false, 0), "ρ | build notes");
        assert_eq!(
            build_terminal_title(Some("build notes"), true, 1),
            "⠙ ρ | build notes"
        );
    }

    #[test]
    fn build_terminal_title_falls_back_for_unnamed_sessions() {
        assert_eq!(build_terminal_title(None, false, 0), "ρ");
        assert_eq!(build_terminal_title(Some(" Untitled session "), true, 0), "⠋ ρ");
    }

    #[test]
    fn sanitize_strips_control_bytes_and_caps_length() {
        let malicious = format!("\x1b]0;bad\x07\n{}", "x".repeat(MAX_TERMINAL_TITLE_LENGTH));
        let sanitized = sanitize_terminal_title(Some(&malicious));
        assert!(!sanitized.contains('\x1b'));
        assert!(!sanitized.contains('\x07'));
        assert!(!sanitized.contains('\n'));
        assert_eq!(pystr::char_len(&sanitized), MAX_TERMINAL_TITLE_LENGTH);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn osc_sequence_sanitizes_payload() {
        assert_eq!(osc_terminal_title_sequence("hello\x07"), "\x1b]0;hello\x07");
    }

    #[test]
    fn terminal_title_supported_requires_tty_and_allows_opt_out() {
        assert!(terminal_title_supported(env(&[("TERM", "xterm-256color")]), true));
        assert!(terminal_title_supported(
            env(&[("TERM", "xterm-256color"), ("NO_COLOR", "1")]),
            true
        ));
        assert!(!terminal_title_supported(env(&[("TERM", "xterm-256color")]), false));
        assert!(!terminal_title_supported(
            env(&[("TERM", "xterm-256color"), ("RHO_TERMINAL_TITLE", "0")]),
            true
        ));
        assert!(!terminal_title_supported(env(&[("TERM", "dumb")]), true));
    }

    #[test]
    fn controller_writes_running_idle_and_restore_titles() {
        let writes = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = writes.clone();
        let mut controller = TerminalTitleController::with_writer(
            true,
            Box::new(move |s: &str| {
                sink.lock().unwrap().push(s.to_string());
                Ok(())
            }),
        );
        controller.update(Some("build notes"), false, 0);
        controller.update(Some("build notes"), false, 0);
        controller.update(Some("build notes"), true, 2);
        controller.restore();
        assert_eq!(
            *writes.lock().unwrap(),
            vec![
                "\x1b]0;ρ | build notes\x07".to_string(),
                "\x1b]0;⠹ ρ | build notes\x07".to_string(),
                "\x1b]0;ρ\x07".to_string(),
            ]
        );
    }

    #[test]
    fn controller_noops_when_disabled() {
        let writes = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = writes.clone();
        let mut controller = TerminalTitleController::with_writer(
            false,
            Box::new(move |s: &str| {
                sink.lock().unwrap().push(s.to_string());
                Ok(())
            }),
        );
        controller.update(Some("build notes"), true, 0);
        controller.restore();
        assert!(writes.lock().unwrap().is_empty());
    }

    #[test]
    fn controller_disables_itself_after_write_failure() {
        let calls = Arc::new(Mutex::new(0u32));
        let counter = calls.clone();
        let mut controller = TerminalTitleController::with_writer(
            true,
            Box::new(move |_s: &str| {
                *counter.lock().unwrap() += 1;
                Err(std::io::Error::other("terminal is gone"))
            }),
        );
        controller.update(Some("build notes"), false, 0);
        controller.update(Some("other"), false, 0);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(!controller.enabled);
    }
}
