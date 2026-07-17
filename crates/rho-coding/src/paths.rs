//! Canonical filesystem paths for rho user and project data.
//!
//! Port of tau's `tau_coding/paths.py`. `TauPaths` becomes [`RhoPaths`]; the
//! home directory defaults to `$RHO_HOME` when set (a rho-specific override),
//! otherwise `$HOME/.rho`, mirroring tau's `Path.home() / ".tau"`.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Return the user's home directory (`$HOME`), empty if unset.
///
/// Mirrors Python's `Path.home()`: the value is **not** symlink-resolved, so it
/// matches tau's slug/relative-home behaviour byte-for-byte.
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Default home for rho user data: `$RHO_HOME` if set, else `$HOME/.rho`.
fn default_home() -> PathBuf {
    if let Some(rho_home) = std::env::var_os("RHO_HOME") {
        PathBuf::from(rho_home)
    } else {
        home_dir().join(".rho")
    }
}

/// Default `.agents` home: `$HOME/.agents`.
fn default_agents_home() -> PathBuf {
    home_dir().join(".agents")
}

/// Resolve a path the way Python's `Path.resolve(strict=False)` does.
///
/// Prefers [`std::fs::canonicalize`] (symlink-resolving, matches `CPython` on
/// existing paths); when the path does not exist it falls back to a lexical
/// absolutise so non-existent cwds still yield a stable absolute path.
pub(crate) fn resolve_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| absolutize(path))
}

fn absolutize(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |dir| dir.join(path))
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolved rho filesystem locations.
///
/// rho keeps durable application data under the user's home directory while
/// also loading project-local resources from the active working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhoPaths {
    /// User-level rho home (durable application data).
    pub home: PathBuf,
    /// User-level `.agents` home.
    pub agents_home: PathBuf,
}

impl Default for RhoPaths {
    fn default() -> Self {
        Self {
            home: default_home(),
            agents_home: default_agents_home(),
        }
    }
}

impl RhoPaths {
    /// Build paths from an explicit home and `.agents` home.
    #[must_use]
    pub fn new(home: PathBuf, agents_home: PathBuf) -> Self {
        Self { home, agents_home }
    }

    /// The user-level session directory.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }

    /// rho's user-level diagnostic log directory.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    /// The JSONL diagnostic log for agent-call failures.
    #[must_use]
    pub fn agent_calls_log_path(&self) -> PathBuf {
        self.logs_dir().join("agent-calls.jsonl")
    }

    /// rho's user-level skills directory.
    #[must_use]
    pub fn user_skills_dir(&self) -> PathBuf {
        self.home.join("skills")
    }

    /// rho's user-level prompt templates directory.
    #[must_use]
    pub fn user_prompts_dir(&self) -> PathBuf {
        self.home.join("prompts")
    }

    /// The user-level `.agents/skills` directory.
    #[must_use]
    pub fn user_agents_skills_dir(&self) -> PathBuf {
        self.agents_home.join("skills")
    }

    /// The user-level `.agents/prompts` directory.
    #[must_use]
    pub fn user_agents_prompts_dir(&self) -> PathBuf {
        self.agents_home.join("prompts")
    }

    /// The project-local rho resource directory (`cwd/.rho`).
    #[must_use]
    pub fn project_rho_dir(&self, cwd: &Path) -> PathBuf {
        cwd.join(".rho")
    }

    /// The project-local `.agents` resource directory.
    #[must_use]
    pub fn project_agents_dir(&self, cwd: &Path) -> PathBuf {
        cwd.join(".agents")
    }

    /// The project-local rho skills directory.
    #[must_use]
    pub fn project_skills_dir(&self, cwd: &Path) -> PathBuf {
        self.project_rho_dir(cwd).join("skills")
    }

    /// The project-local rho prompt templates directory.
    #[must_use]
    pub fn project_prompts_dir(&self, cwd: &Path) -> PathBuf {
        self.project_rho_dir(cwd).join("prompts")
    }

    /// The project-local `.agents/skills` directory.
    #[must_use]
    pub fn project_agents_skills_dir(&self, cwd: &Path) -> PathBuf {
        self.project_agents_dir(cwd).join("skills")
    }

    /// The project-local `.agents/prompts` directory.
    #[must_use]
    pub fn project_agents_prompts_dir(&self, cwd: &Path) -> PathBuf {
        self.project_agents_dir(cwd).join("prompts")
    }

    /// The user-home session directory for a project cwd.
    #[must_use]
    pub fn project_session_dir(&self, cwd: &Path) -> PathBuf {
        let resolved = resolve_path(cwd);
        let mut hasher = Sha256::new();
        hasher.update(resolved.to_string_lossy().as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        let digest = &digest[..6];
        let slug = slugify_path(&resolved, 72);
        let name = if slug.is_empty() {
            format!("project-{digest}")
        } else {
            format!("{slug}-{digest}")
        };
        self.sessions_dir().join(name)
    }

    /// The default JSONL session path for a project cwd (parent created).
    #[must_use]
    pub fn default_session_path(&self, cwd: &Path) -> PathBuf {
        let path = self.project_session_dir(cwd).join("default.jsonl");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        path
    }
}

/// Slugify a resolved filesystem path for use as a session directory name.
///
/// Faithful port of tau's `_slugify_path`.
fn slugify_path(path: &Path, max_length: usize) -> String {
    let home = home_dir();
    let parts: Vec<String> = if !home.as_os_str().is_empty() && path.starts_with(&home) {
        // Path is under `$HOME`: replace the home prefix with a literal "home".
        let mut collected = vec!["home".to_string()];
        if let Ok(rel) = path.strip_prefix(&home) {
            collected.extend(rel.components().filter_map(normal_component));
        }
        collected
    } else {
        path.components().filter_map(normal_component).collect()
    };

    let slug_parts: Vec<String> = parts
        .iter()
        .map(|part| slug_part(part))
        .filter(|part| !part.is_empty())
        .collect();
    let slug = slug_parts.join("-");
    if slug.len() <= max_length {
        return slug;
    }

    let mut suffix_parts: Vec<&str> = Vec::new();
    let mut suffix_length = 0usize;
    for part in slug_parts.iter().rev() {
        let next_length = suffix_length + part.len() + usize::from(!suffix_parts.is_empty());
        if next_length > max_length {
            break;
        }
        suffix_parts.push(part);
        suffix_length = next_length;
    }
    if suffix_parts.is_empty() {
        let start = slug.len().saturating_sub(max_length);
        slug[start..].trim_matches('-').to_string()
    } else {
        suffix_parts.reverse();
        suffix_parts.join("-")
    }
}

/// Extract the string form of a `Normal` path component (skips root/prefix).
fn normal_component(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Normalise a single path segment: replace runs of characters outside
/// `[a-zA-Z0-9._-]` with a single `-`, strip leading/trailing `.-_`, lowercase.
fn slug_part(part: &str) -> String {
    let mut normalized = String::with_capacity(part.len());
    let mut pending_dash = false;
    for ch in part.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            normalized.push(ch);
            pending_dash = false;
        } else if !pending_dash {
            normalized.push('-');
            pending_dash = true;
        }
    }
    normalized
        .trim_matches(|c| c == '.' || c == '-' || c == '_')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rho_paths_user_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let paths = RhoPaths::new(base.join(".rho"), base.join(".agents"));

        assert_eq!(paths.sessions_dir(), base.join(".rho").join("sessions"));
        assert_eq!(paths.user_skills_dir(), base.join(".rho").join("skills"));
        assert_eq!(paths.user_prompts_dir(), base.join(".rho").join("prompts"));
        assert_eq!(
            paths.user_agents_skills_dir(),
            base.join(".agents").join("skills")
        );
        assert_eq!(
            paths.user_agents_prompts_dir(),
            base.join(".agents").join("prompts")
        );
    }

    #[test]
    fn rho_paths_project_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let paths = RhoPaths::new(base.join("home"), base.join("agents"));
        let cwd = base.join("project");

        assert_eq!(paths.project_rho_dir(&cwd), cwd.join(".rho"));
        assert_eq!(paths.project_agents_dir(&cwd), cwd.join(".agents"));
        assert_eq!(
            paths.project_skills_dir(&cwd),
            cwd.join(".rho").join("skills")
        );
        assert_eq!(
            paths.project_prompts_dir(&cwd),
            cwd.join(".rho").join("prompts")
        );
        assert_eq!(
            paths.project_agents_skills_dir(&cwd),
            cwd.join(".agents").join("skills")
        );
        assert_eq!(
            paths.project_agents_prompts_dir(&cwd),
            cwd.join(".agents").join("prompts")
        );
    }

    #[test]
    fn default_session_path_uses_home_sessions_and_readable_project_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let paths = RhoPaths::new(base.join("home"), base.join("agents"));
        let cwd = base.join("repos").join("exploration").join("tau");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_path = paths.default_session_path(&cwd);

        assert_eq!(session_path.file_name().unwrap(), "default.jsonl");
        assert_eq!(
            session_path.parent().unwrap().parent().unwrap(),
            base.join("home").join("sessions")
        );
        let dir_name = session_path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(dir_name.contains("repos-exploration-tau-"), "{dir_name}");
        let (_, digest) = dir_name.rsplit_once('-').unwrap();
        assert_eq!(digest.len(), 6);
        assert!(session_path.parent().unwrap().exists());
    }

    #[test]
    fn slug_part_normalizes_and_lowercases() {
        assert_eq!(slug_part("My Repo!!"), "my-repo");
        assert_eq!(slug_part("__leading._"), "leading");
        assert_eq!(
            slug_part("keep.dots-and_underscores"),
            "keep.dots-and_underscores"
        );
    }
}
