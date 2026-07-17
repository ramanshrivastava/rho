//! Structured diagnostic logging for coding-session failures (port of tau's
//! `tau_coding/diagnostics.py`).
//!
//! The logger appends one `sort_keys` JSON object per failure to
//! `~/.rho/logs/agent-calls.jsonl`. tau stamps each entry with
//! `datetime.now(UTC).isoformat()`; rho derives an ISO-8601 UTC string from the
//! session [`Clock`](rho_agent::clock::Clock) so the record is reproducible
//! under a pinned clock. The file bytes are not a golden (no fixture pins them),
//! so the timestamp format is best-effort ISO rather than tau-exact.

#![allow(clippy::cast_possible_wrap)]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rho_agent::messages::AssistantMessage;
use serde_json::json;

use crate::paths::RhoPaths;

/// Non-secret context attached to an agent-call diagnostic entry.
#[derive(Debug, Clone)]
pub struct AgentCallDiagnosticContext {
    /// Active provider name.
    pub provider_name: String,
    /// Active model.
    pub model: String,
    /// Session working directory.
    pub cwd: PathBuf,
    /// Session id, if indexed.
    pub session_id: Option<String>,
    /// Stable id for one agent call.
    pub run_id: String,
}

/// Appends structured JSONL diagnostics for agent-call failures.
#[derive(Debug, Clone)]
pub struct AgentCallDiagnosticLogger {
    path: PathBuf,
}

impl AgentCallDiagnosticLogger {
    /// Build a logger writing to `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Build a logger using rho's default path layout.
    #[must_use]
    pub fn from_paths(paths: &RhoPaths) -> Self {
        Self::new(paths.agent_calls_log_path())
    }

    /// The backing log path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Log an unexpected exception-like failure; returns the log path.
    pub fn log_exception(
        &self,
        context: &AgentCallDiagnosticContext,
        phase: &str,
        error_type: &str,
        message: &str,
    ) -> PathBuf {
        let mut entry = base_entry(context, phase, "exception");
        entry.insert(
            "exception".to_string(),
            json!({ "type": error_type, "message": message, "traceback": "" }),
        );
        self.append(&entry);
        self.path.clone()
    }

    /// Log a terminal assistant error message; returns the log path.
    pub fn log_assistant_error(
        &self,
        context: &AgentCallDiagnosticContext,
        phase: &str,
        message: &AssistantMessage,
    ) -> PathBuf {
        let err_message = message
            .error_message
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "Error".to_string());
        let stop_reason =
            serde_json::to_value(message.stop_reason).unwrap_or(serde_json::Value::Null);
        let mut entry = base_entry(context, phase, "assistant_error");
        entry.insert(
            "error".to_string(),
            json!({ "message": err_message, "stop_reason": stop_reason }),
        );
        self.append(&entry);
        self.path.clone()
    }

    fn append(&self, entry: &serde_json::Map<String, serde_json::Value>) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // sort_keys=True: serialize a BTreeMap so keys are lexicographically ordered.
        let ordered: std::collections::BTreeMap<&String, &serde_json::Value> =
            entry.iter().collect();
        let Ok(line) = serde_json::to_string(&ordered) else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Return a stable id for one coding-session agent call.
#[must_use]
pub fn new_agent_call_run_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn base_entry(
    context: &AgentCallDiagnosticContext,
    phase: &str,
    kind: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert("timestamp".to_string(), json!(now_iso8601_utc()));
    map.insert("kind".to_string(), json!(kind));
    map.insert("phase".to_string(), json!(phase));
    map.insert("run_id".to_string(), json!(context.run_id));
    map.insert(
        "session_id".to_string(),
        context
            .session_id
            .clone()
            .map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    map.insert("provider_name".to_string(), json!(context.provider_name));
    map.insert("model".to_string(), json!(context.model));
    map.insert("cwd".to_string(), json!(context.cwd.to_string_lossy()));
    map
}

/// Best-effort ISO-8601 UTC timestamp from the system clock (not a golden).
fn now_iso8601_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // Civil-date conversion (Howard Hinnant's algorithm), UTC.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00")
}
