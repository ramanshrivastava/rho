//! User-home session management for rho coding sessions.
//!
//! Port of tau's `tau_coding/session_manager.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths::{RhoPaths, resolve_path};

/// A session-index read failure: a malformed / schema-invalid index line, or an
/// unreadable index file. tau lets pydantic's `ValidationError` propagate out of
/// `_read_index`; rho surfaces the same failure so a corrupt index makes the CLI
/// exit non-zero instead of silently dropping records.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SessionManagerError(pub String);

/// Current unix time in seconds as a float (Python `time.time()`).
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// JSON-serializable coding-session metadata.
///
/// Field order matches tau's pydantic model (byte-compat with
/// `model_dump_json`). `None` optionals serialize as JSON `null` (tau does not
/// pass `exclude_none` on this path); unknown fields are ignored on read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecordModel {
    /// Session id.
    pub id: String,
    /// Session JSONL path (stringified).
    pub path: String,
    /// Resolved working directory (stringified).
    pub cwd: String,
    /// Model identifier.
    pub model: String,
    /// Provider name, or `null`.
    #[serde(default)]
    pub provider_name: Option<String>,
    /// Human title, or `null`.
    #[serde(default)]
    pub title: Option<String>,
    /// Creation time (unix seconds).
    pub created_at: f64,
    /// Last-updated time (unix seconds).
    pub updated_at: f64,
}

/// Metadata for one durable coding session.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingSessionRecord {
    /// Session id.
    pub id: String,
    /// Session JSONL path.
    pub path: PathBuf,
    /// Resolved working directory.
    pub cwd: PathBuf,
    /// Model identifier.
    pub model: String,
    /// Human title, if any.
    pub title: Option<String>,
    /// Creation time (unix seconds).
    pub created_at: f64,
    /// Last-updated time (unix seconds).
    pub updated_at: f64,
    /// Provider name, if any.
    pub provider_name: Option<String>,
}

impl CodingSessionRecord {
    /// Convert a JSON model to a record.
    #[must_use]
    pub fn from_model(model: SessionRecordModel) -> Self {
        Self {
            id: model.id,
            path: PathBuf::from(model.path),
            cwd: PathBuf::from(model.cwd),
            model: model.model,
            title: model.title,
            created_at: model.created_at,
            updated_at: model.updated_at,
            provider_name: model.provider_name,
        }
    }

    /// Convert this record to a JSON model.
    #[must_use]
    pub fn to_model(&self) -> SessionRecordModel {
        SessionRecordModel {
            id: self.id.clone(),
            path: self.path.to_string_lossy().into_owned(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            model: self.model.clone(),
            provider_name: self.provider_name.clone(),
            title: self.title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Create, index, list, and resume user-home coding sessions.
#[derive(Debug, Clone, Default)]
pub struct SessionManager {
    /// Filesystem locations backing this manager.
    pub paths: RhoPaths,
}

impl SessionManager {
    /// Build a session manager over the given paths.
    #[must_use]
    pub fn new(paths: RhoPaths) -> Self {
        Self { paths }
    }

    /// The legacy global session metadata index path.
    #[must_use]
    pub fn index_path(&self) -> PathBuf {
        self.paths.sessions_dir().join("index.jsonl")
    }

    /// The session metadata index path for a project cwd.
    #[must_use]
    pub fn project_index_path(&self, cwd: &Path) -> PathBuf {
        self.paths.project_session_dir(cwd).join("index.jsonl")
    }

    /// Return indexed sessions, newest updated first.
    ///
    /// With `cwd`, only sessions for that resolved working directory are
    /// returned; without it, records aggregate across project indexes and the
    /// legacy global index.
    ///
    /// # Errors
    /// Returns [`SessionManagerError`] if any index file contains a malformed /
    /// schema-invalid line (tau parity: a corrupt index is fatal).
    pub fn list_sessions(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<CodingSessionRecord>, SessionManagerError> {
        let mut records = match cwd {
            Some(cwd) => self.read_project_records(cwd)?,
            None => self.read_all_records()?,
        };
        // Stable descending sort by `updated_at` (matches Python's stable
        // `sorted(..., reverse=True)`).
        records.sort_by(|a, b| {
            b.updated_at
                .partial_cmp(&a.updated_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(records)
    }

    /// Return a session record by id, if present.
    ///
    /// # Errors
    /// Returns [`SessionManagerError`] if the index contains a corrupt line.
    pub fn get_session(
        &self,
        session_id: &str,
    ) -> Result<Option<CodingSessionRecord>, SessionManagerError> {
        Ok(self
            .read_all_records()?
            .into_iter()
            .find(|record| record.id == session_id))
    }

    /// Return the most recently updated session for a working directory.
    ///
    /// # Errors
    /// Returns [`SessionManagerError`] if the index contains a corrupt line.
    pub fn latest_session_for_cwd(
        &self,
        cwd: &Path,
    ) -> Result<Option<CodingSessionRecord>, SessionManagerError> {
        Ok(self.list_sessions(Some(cwd))?.into_iter().next())
    }

    /// Create and index a new session record.
    #[must_use]
    pub fn create_session(
        &self,
        cwd: &Path,
        model: &str,
        provider_name: Option<&str>,
        title: Option<&str>,
        session_id: Option<&str>,
    ) -> CodingSessionRecord {
        let record = self.prepare_session(cwd, model, provider_name, title, session_id);
        self.index_session(&record);
        record
    }

    /// Return metadata for a session without adding it to the resume index.
    #[must_use]
    pub fn prepare_session(
        &self,
        cwd: &Path,
        model: &str,
        provider_name: Option<&str>,
        title: Option<&str>,
        session_id: Option<&str>,
    ) -> CodingSessionRecord {
        let now = now_seconds();
        let resolved_cwd = resolve_path(cwd);
        let record_id =
            session_id.map_or_else(|| Uuid::new_v4().simple().to_string(), ToString::to_string);
        let path = self
            .paths
            .project_session_dir(&resolved_cwd)
            .join(format!("{record_id}.jsonl"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        CodingSessionRecord {
            id: record_id,
            path,
            cwd: resolved_cwd,
            model: model.to_string(),
            provider_name: provider_name.map(ToString::to_string),
            title: title.map(ToString::to_string),
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a prepared session record to the resume index.
    pub fn index_session(&self, record: &CodingSessionRecord) {
        self.upsert(record);
    }

    /// Return the default project session, creating an index record when needed.
    #[must_use]
    pub fn get_or_create_default_session(
        &self,
        cwd: &Path,
        model: &str,
        provider_name: Option<&str>,
    ) -> CodingSessionRecord {
        let resolved_cwd = resolve_path(cwd);
        let project_hash = self
            .paths
            .project_session_dir(&resolved_cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session_id = format!("default-{project_hash}");
        if let Some(existing) = self.get_session(&session_id).ok().flatten() {
            return existing;
        }

        let now = now_seconds();
        let path = self.paths.default_session_path(&resolved_cwd);
        let record = CodingSessionRecord {
            id: session_id,
            path,
            cwd: resolved_cwd,
            model: model.to_string(),
            provider_name: provider_name.map(ToString::to_string),
            title: Some("Default session".to_string()),
            created_at: now,
            updated_at: now,
        };
        self.upsert(&record);
        record
    }

    /// Update a session's last-used metadata.
    ///
    /// `None` arguments leave the corresponding field unchanged.
    #[must_use]
    pub fn touch_session(
        &self,
        session_id: &str,
        model: Option<&str>,
        provider_name: Option<&str>,
        title: Option<&str>,
    ) -> Option<CodingSessionRecord> {
        let existing = self.get_session(session_id).ok().flatten()?;
        let updated = CodingSessionRecord {
            id: existing.id.clone(),
            path: existing.path.clone(),
            cwd: existing.cwd.clone(),
            model: model.map_or_else(|| existing.model.clone(), ToString::to_string),
            provider_name: provider_name
                .map_or_else(|| existing.provider_name.clone(), |v| Some(v.to_string())),
            title: title.map_or_else(|| existing.title.clone(), |v| Some(v.to_string())),
            created_at: existing.created_at,
            updated_at: now_seconds(),
        };
        self.upsert(&updated);
        Some(updated)
    }

    fn read_index(path: &Path) -> Result<Vec<CodingSessionRecord>, SessionManagerError> {
        // tau `_read_index`: a missing file is empty, but a malformed/schema-
        // invalid line raises `ValidationError` and propagates. Match that — a
        // corrupt index is a hard error, not a silent per-line drop.
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(path).map_err(|err| {
            SessionManagerError(format!(
                "Failed to read session index {}: {err}",
                path.display()
            ))
        })?;
        let mut records = Vec::new();
        // Split on newlines only: Python's str.splitlines() (and Rust's
        // str::lines()) would also break on characters like U+2028 that appear
        // unescaped inside JSON string values. tau splits on "\n" alone here.
        for line in text.split('\n') {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            let model = serde_json::from_str::<SessionRecordModel>(stripped).map_err(|err| {
                SessionManagerError(format!(
                    "Invalid session index entry in {}: {err}",
                    path.display()
                ))
            })?;
            records.push(CodingSessionRecord::from_model(model));
        }
        Ok(records)
    }

    fn read_project_records(
        &self,
        cwd: &Path,
    ) -> Result<Vec<CodingSessionRecord>, SessionManagerError> {
        let resolved_cwd = resolve_path(cwd);
        let mut records = Self::read_index(&self.project_index_path(&resolved_cwd))?;
        records.extend(
            Self::read_index(&self.index_path())?
                .into_iter()
                .filter(|record| record.cwd == resolved_cwd),
        );
        Ok(deduplicate_records(records))
    }

    fn read_all_records(&self) -> Result<Vec<CodingSessionRecord>, SessionManagerError> {
        let mut records = Self::read_index(&self.index_path())?;
        if let Ok(entries) = std::fs::read_dir(self.paths.sessions_dir()) {
            for entry in entries.flatten() {
                let index_path = entry.path().join("index.jsonl");
                if index_path.is_file() {
                    records.extend(Self::read_index(&index_path)?);
                }
            }
        }
        Ok(deduplicate_records(records))
    }

    fn write_index(path: &Path, records: &[CodingSessionRecord]) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut content = records
            .iter()
            .map(|record| serde_json::to_string(&record.to_model()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        let _ = std::fs::write(path, content);
    }

    fn upsert(&self, record: &CodingSessionRecord) {
        let path = self.project_index_path(&record.cwd);
        // Write path is best-effort: if the existing index can't be parsed we
        // start from what we can read rather than abort a write. (Strict parsing
        // is enforced on the read/query APIs, which is where tau's fatal
        // `ValidationError` is user-visible.)
        let mut records: Vec<CodingSessionRecord> = Self::read_index(&path)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.id != record.id)
            .collect();
        records.push(record.clone());
        Self::write_index(&path, &records);
    }
}

/// Keep the newest record per id, preserving first-seen order (tau parity).
fn deduplicate_records(records: Vec<CodingSessionRecord>) -> Vec<CodingSessionRecord> {
    let mut order: Vec<CodingSessionRecord> = Vec::new();
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    for record in records {
        if let Some(&idx) = index_by_id.get(&record.id) {
            if record.updated_at >= order[idx].updated_at {
                order[idx] = record;
            }
        } else {
            index_by_id.insert(record.id.clone(), order.len());
            order.push(record);
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(base: &Path) -> SessionManager {
        SessionManager {
            paths: RhoPaths::new(base.join(".rho"), base.join(".agents")),
        }
    }

    #[test]
    fn creates_and_lists_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();

        let record =
            manager.create_session(&cwd, "fake", Some("fake-provider"), Some("Test"), None);

        assert_eq!(record.provider_name.as_deref(), Some("fake-provider"));
        assert_eq!(
            record.path.parent().unwrap().parent().unwrap(),
            base.join(".rho").join("sessions")
        );
        let dir_name = record
            .path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(dir_name.contains("project-"), "{dir_name}");
        assert_eq!(dir_name.rsplit_once('-').unwrap().1.len(), 6);
        assert!(record.path.parent().unwrap().join("index.jsonl").exists());
        assert!(
            !base
                .join(".rho")
                .join("sessions")
                .join("index.jsonl")
                .exists()
        );
        assert_eq!(
            record.path.file_name().unwrap().to_string_lossy(),
            format!("{}.jsonl", record.id)
        );
        assert_eq!(
            manager.get_session(&record.id).unwrap(),
            Some(record.clone())
        );
        assert_eq!(manager.list_sessions(None).unwrap(), vec![record.clone()]);
        assert_eq!(manager.list_sessions(Some(&cwd)).unwrap(), vec![record]);
    }

    #[test]
    fn prepares_unindexed_session() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();

        let record = manager.prepare_session(&cwd, "fake", Some("fake-provider"), None, None);

        assert_eq!(record.provider_name.as_deref(), Some("fake-provider"));
        assert_eq!(
            record.path.file_name().unwrap().to_string_lossy(),
            format!("{}.jsonl", record.id)
        );
        assert_eq!(manager.get_session(&record.id).unwrap(), None);
        assert_eq!(manager.list_sessions(Some(&cwd)).unwrap(), Vec::new());

        manager.index_session(&record);

        assert_eq!(
            manager.get_session(&record.id).unwrap(),
            Some(record.clone())
        );
        assert_eq!(manager.list_sessions(Some(&cwd)).unwrap(), vec![record]);
    }

    #[test]
    fn filters_sessions_by_project_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let first_cwd = base.join("first");
        let second_cwd = base.join("second");
        std::fs::create_dir(&first_cwd).unwrap();
        std::fs::create_dir(&second_cwd).unwrap();

        let first = manager.create_session(&first_cwd, "fake", None, Some("First"), None);
        let second = manager.create_session(&second_cwd, "fake", None, Some("Second"), None);

        assert_eq!(
            manager.list_sessions(Some(&first_cwd)).unwrap(),
            vec![first.clone()]
        );
        assert_eq!(
            manager.list_sessions(Some(&second_cwd)).unwrap(),
            vec![second.clone()]
        );
        let ids: std::collections::HashSet<String> = manager
            .list_sessions(None)
            .unwrap()
            .into_iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(ids, std::collections::HashSet::from([first.id, second.id]));
    }

    #[test]
    fn returns_latest_session_for_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();
        let older = manager.create_session(&cwd, "older", None, None, Some("older"));
        let newer = manager.create_session(&cwd, "newer", None, None, Some("newer"));
        let _ = manager.touch_session(&older.id, None, None, None);

        let latest = manager.latest_session_for_cwd(&cwd).unwrap().unwrap();

        assert_eq!(latest.id, older.id);
        assert_eq!(latest.model, "older");
        assert!(
            manager
                .list_sessions(Some(&cwd))
                .unwrap()
                .iter()
                .any(|r| r.id == newer.id)
        );
    }

    #[test]
    fn ignores_extra_index_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();
        let resolved_cwd = resolve_path(&cwd);
        let index_path = manager.project_index_path(&cwd);
        let session_path = index_path.parent().unwrap().join("session-1.jsonl");
        std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        let line = serde_json::json!({
            "id": "session-1",
            "path": session_path.to_string_lossy(),
            "cwd": resolved_cwd.to_string_lossy(),
            "model": "gpt-5",
            "title": "Session",
            "created_at": 1.0,
            "updated_at": 2.0,
            "provider_name": "openai-codex",
        });
        std::fs::write(&index_path, format!("{line}\n")).unwrap();

        let records = manager.list_sessions(Some(&cwd)).unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.id, "session-1");
        assert_eq!(record.path, session_path);
        assert_eq!(record.model, "gpt-5");
    }

    #[test]
    fn gets_or_creates_default_session() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();

        let first = manager.get_or_create_default_session(&cwd, "fake", Some("fake-provider"));
        let second = manager.get_or_create_default_session(&cwd, "other", None);

        assert_eq!(first, second);
        assert_eq!(first.provider_name.as_deref(), Some("fake-provider"));
        assert!(first.id.starts_with("default-"));
        assert_eq!(
            first.path.file_name().unwrap().to_string_lossy(),
            "default.jsonl"
        );
        assert!(first.path.parent().unwrap().exists());
    }

    #[test]
    fn touch_updates_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();
        let record = manager.create_session(&cwd, "fake", None, None, None);

        let updated = manager
            .touch_session(
                &record.id,
                Some("new-model"),
                Some("new-provider"),
                Some("Updated"),
            )
            .unwrap();

        assert_eq!(updated.id, record.id);
        assert_eq!(updated.model, "new-model");
        assert_eq!(updated.provider_name.as_deref(), Some("new-provider"));
        assert_eq!(updated.title.as_deref(), Some("Updated"));
        assert!(updated.updated_at >= record.updated_at);
        assert_eq!(manager.get_session(&record.id).unwrap(), Some(updated));
    }

    #[test]
    fn sorts_newest_updated_first() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();
        let older = manager.create_session(&cwd, "fake", None, None, Some("older"));
        let _newer = manager.create_session(&cwd, "fake", None, None, Some("newer"));
        let _ = manager.touch_session(&older.id, None, None, None);

        let sessions = manager.list_sessions(None).unwrap();
        let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["older".to_string(), "newer".to_string()]);
    }

    #[test]
    fn corrupt_index_line_is_fatal() {
        // tau `_read_index` lets pydantic's `ValidationError` propagate; a
        // malformed/schema-invalid index line must be a hard error, not a
        // silently-dropped record.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let manager = manager(base);
        let cwd = base.join("project");
        std::fs::create_dir(&cwd).unwrap();

        // Seed a good record, then append a corrupt line to the same index.
        let record = manager.create_session(&cwd, "fake", None, None, None);
        let index_path = manager.project_index_path(&cwd);
        let mut contents = std::fs::read_to_string(&index_path).unwrap();
        contents.push_str("{ not valid json\n");
        std::fs::write(&index_path, contents).unwrap();

        assert!(
            manager.list_sessions(Some(&cwd)).is_err(),
            "a corrupt project index fails the read"
        );
        assert!(
            manager.get_session(&record.id).is_err(),
            "a corrupt index fails a by-id lookup too"
        );

        // A schema-invalid line (valid JSON, missing required fields) is equally
        // fatal — matching pydantic validation, not just JSON syntax.
        std::fs::write(&index_path, "{\"id\":\"only-id\"}\n").unwrap();
        assert!(manager.list_sessions(Some(&cwd)).is_err());
    }
}
