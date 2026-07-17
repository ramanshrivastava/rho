//! Markdown resource path and frontmatter helpers.
//!
//! Port of tau's `tau_coding/resources.py` (`RhoResourcePaths`,
//! `ResourceDiagnostic`, `ResourceError`, `resource_paths_with_cwd`,
//! `parse_markdown_resource`, `derive_description`, `metadata_to_json`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::paths::{RhoPaths, home_dir};

/// Raised when rho resources are invalid or cannot be expanded.
///
/// Ports tau's `ResourceError(ValueError)`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ResourceError(pub String);

/// A non-fatal resource discovery problem or precedence note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDiagnostic {
    /// Diagnostic category (e.g. `"context"`).
    pub kind: String,
    /// Human-readable message.
    pub message: String,
    /// Optional filesystem path the diagnostic refers to.
    pub path: Option<PathBuf>,
    /// Optional resource name.
    pub name: Option<String>,
    /// Severity label (defaults to `"warning"`).
    pub severity: String,
}

impl ResourceDiagnostic {
    /// Build a warning-severity diagnostic with `kind` and `message`.
    #[must_use]
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            path: None,
            name: None,
            severity: "warning".to_string(),
        }
    }

    /// Attach a path to the diagnostic.
    #[must_use]
    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    /// Attach a name to the diagnostic.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Override the severity label.
    #[must_use]
    pub fn with_severity(mut self, severity: impl Into<String>) -> Self {
        self.severity = severity.into();
        self
    }

    /// Return a concise human-readable diagnostic line.
    #[must_use]
    pub fn format(&self) -> String {
        let mut parts = vec![self.severity.clone(), self.kind.clone()];
        if let Some(name) = &self.name {
            parts.push(name.clone());
        }
        let label = parts.join(" ");
        match &self.path {
            None => format!("{label}: {}", self.message),
            Some(path) => format!("{label}: {} ({})", self.message, path.display()),
        }
    }
}

/// Filesystem locations for rho markdown resources.
///
/// By default rho loads both rho-native resources and `.agents` resources from
/// the user home directory. When a cwd is provided, project-local `.rho` and
/// `.agents` resources are loaded automatically as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhoResourcePaths {
    /// Primary resource root (defaults to `$HOME/.rho`).
    pub root: PathBuf,
    /// Optional active working directory for project-local discovery.
    pub cwd: Option<PathBuf>,
    /// Optional `.agents` root (defaults to `$HOME/.agents`).
    pub agents_root: Option<PathBuf>,
    /// Optional explicit [`RhoPaths`] override.
    pub paths: Option<RhoPaths>,
}

impl Default for RhoResourcePaths {
    fn default() -> Self {
        Self {
            root: home_dir().join(".rho"),
            cwd: None,
            agents_root: Some(home_dir().join(".agents")),
            paths: None,
        }
    }
}

impl RhoResourcePaths {
    /// The primary rho skills directory.
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// The primary rho prompt templates directory.
    #[must_use]
    pub fn prompts_dir(&self) -> PathBuf {
        self.root.join("prompts")
    }

    /// Skill directories in increasing precedence order.
    ///
    /// Only the `skills` subdirectory of an `.agents` root is scanned, never the
    /// root `.agents` directory itself (which may hold `README.md`, `AGENTS.md`).
    #[must_use]
    pub fn skills_dirs(&self) -> Vec<PathBuf> {
        let paths = self.resolved_paths();
        let mut dirs = vec![self.skills_dir()];
        if let Some(agents_root) = &self.agents_root {
            dirs.push(agents_root.join("skills"));
        }
        if let Some(cwd) = &self.cwd {
            dirs.push(paths.project_skills_dir(cwd));
            dirs.push(paths.project_agents_skills_dir(cwd));
        }
        dedupe_paths(dirs)
    }

    /// Prompt template directories in increasing precedence order.
    #[must_use]
    pub fn prompts_dirs(&self) -> Vec<PathBuf> {
        let paths = self.resolved_paths();
        let mut dirs = vec![self.prompts_dir()];
        if let Some(agents_root) = &self.agents_root {
            dirs.push(agents_root.join("prompts"));
        }
        if let Some(cwd) = &self.cwd {
            dirs.push(paths.project_prompts_dir(cwd));
            dirs.push(paths.project_agents_prompts_dir(cwd));
        }
        dedupe_paths(dirs)
    }

    /// Resolve the effective [`RhoPaths`] (tau's private `_paths`).
    #[must_use]
    pub(crate) fn resolved_paths(&self) -> RhoPaths {
        if let Some(paths) = &self.paths {
            return paths.clone();
        }
        let agents_home = self
            .agents_root
            .clone()
            .unwrap_or_else(|| home_dir().join(".agents"));
        RhoPaths::new(self.root.clone(), agents_home)
    }
}

/// Deduplicate paths (comparing `expanduser`-expanded forms), preserving order.
fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut deduped: Vec<PathBuf> = Vec::new();
    for path in paths {
        let resolved = expanduser(&path);
        if seen.insert(resolved) {
            deduped.push(path);
        }
    }
    deduped
}

/// Minimal `Path.expanduser()`: expand a leading `~` to `$HOME`.
fn expanduser(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

/// Return resource paths with a cwd available for project-local discovery.
#[must_use]
pub fn resource_paths_with_cwd(paths: Option<RhoResourcePaths>, cwd: &Path) -> RhoResourcePaths {
    match paths {
        None => RhoResourcePaths {
            cwd: Some(cwd.to_path_buf()),
            ..RhoResourcePaths::default()
        },
        Some(paths) => {
            if paths.cwd.is_some() {
                return paths;
            }
            RhoResourcePaths {
                root: paths.root,
                cwd: Some(cwd.to_path_buf()),
                agents_root: paths.agents_root,
                paths: paths.paths,
            }
        }
    }
}

/// Parse minimal YAML-like frontmatter from a markdown resource.
///
/// Only simple `key: value` pairs are supported. Returns `(metadata, body)`.
///
/// Note: tau returns a `dict` preserving insertion order; here metadata is a
/// [`BTreeMap`] (key-sorted). The only downstream consumer treats it as a plain
/// key/value lookup, so ordering is not load-bearing.
#[must_use]
pub fn parse_markdown_resource(text: &str) -> (BTreeMap<String, String>, String) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---\n") {
        return (BTreeMap::new(), normalized);
    }

    let Some(rel_end) = normalized[4..].find("\n---") else {
        return (BTreeMap::new(), normalized);
    };
    let end = rel_end + 4;

    let raw_frontmatter = &normalized[4..end];
    let mut body = &normalized[end + "\n---".len()..];
    if let Some(stripped) = body.strip_prefix('\n') {
        body = stripped;
    }

    let mut metadata: BTreeMap<String, String> = BTreeMap::new();
    for line in raw_frontmatter.split('\n') {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let Some((key, value)) = stripped.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        metadata.insert(key.trim().to_string(), value.to_string());
    }
    (metadata, body.to_string())
}

/// Derive a short description from markdown content.
#[must_use]
pub fn derive_description(content: &str) -> Option<String> {
    for line in content.split('\n') {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        if let Some(rest) = stripped.strip_prefix('#') {
            // `lstrip("#")` removes every leading '#'.
            let mut heading = rest;
            while let Some(next) = heading.strip_prefix('#') {
                heading = next;
            }
            let heading = heading.trim();
            return if heading.is_empty() {
                None
            } else {
                Some(heading.to_string())
            };
        }
        return Some(stripped.to_string());
    }
    None
}

/// Convert string metadata into JSON-like values (tau's `metadata_to_json`).
#[must_use]
pub fn metadata_to_json(
    metadata: &BTreeMap<String, String>,
) -> BTreeMap<String, serde_json::Value> {
    metadata
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_markdown_resource_extracts_frontmatter() {
        let text = "---\nname: demo\ndescription: \"a thing\"\n# comment\n---\nbody line\n";
        let (metadata, body) = parse_markdown_resource(text);
        assert_eq!(metadata.get("name"), Some(&"demo".to_string()));
        assert_eq!(metadata.get("description"), Some(&"a thing".to_string()));
        assert_eq!(body, "body line\n");
    }

    #[test]
    fn parse_markdown_resource_without_frontmatter() {
        let text = "just body\nsecond line\n";
        let (metadata, body) = parse_markdown_resource(text);
        assert!(metadata.is_empty());
        assert_eq!(body, "just body\nsecond line\n");
    }

    #[test]
    fn parse_markdown_resource_unterminated_frontmatter() {
        let text = "---\nname: demo\nno end here\n";
        let (metadata, body) = parse_markdown_resource(text);
        assert!(metadata.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn derive_description_from_heading_and_text() {
        assert_eq!(
            derive_description("\n\n# Title here\nrest"),
            Some("Title here".to_string())
        );
        assert_eq!(
            derive_description("plain first line\nsecond"),
            Some("plain first line".to_string())
        );
        // A heading that reduces to empty returns None immediately (tau does
        // not fall through to the next line).
        assert_eq!(derive_description("###   \nx"), None);
        assert_eq!(
            derive_description("## Heading\n"),
            Some("Heading".to_string())
        );
        assert_eq!(derive_description("   \n\n"), None);
    }

    #[test]
    fn resource_paths_with_cwd_sets_cwd_when_missing() {
        let cwd = PathBuf::from("/tmp/project");
        let result = resource_paths_with_cwd(None, &cwd);
        assert_eq!(result.cwd, Some(cwd.clone()));

        let base = RhoResourcePaths {
            root: PathBuf::from("/root/.rho"),
            cwd: None,
            agents_root: Some(PathBuf::from("/root/.agents")),
            paths: None,
        };
        let result = resource_paths_with_cwd(Some(base.clone()), &cwd);
        assert_eq!(result.cwd, Some(cwd.clone()));
        assert_eq!(result.root, base.root);

        let with_cwd = RhoResourcePaths {
            cwd: Some(PathBuf::from("/other")),
            ..base
        };
        let result = resource_paths_with_cwd(Some(with_cwd.clone()), &cwd);
        assert_eq!(result.cwd, Some(PathBuf::from("/other")));
    }

    #[test]
    fn diagnostic_format_matches_tau() {
        let diag = ResourceDiagnostic::new("context", "could not read");
        assert_eq!(diag.format(), "warning context: could not read");
        let diag = diag.with_path(PathBuf::from("/x/AGENTS.md"));
        assert_eq!(
            diag.format(),
            "warning context: could not read (/x/AGENTS.md)"
        );
    }

    #[test]
    fn skills_dirs_dedupes_and_orders() {
        let paths = RhoResourcePaths {
            root: PathBuf::from("/home/.rho"),
            cwd: Some(PathBuf::from("/proj")),
            agents_root: Some(PathBuf::from("/home/.agents")),
            paths: None,
        };
        let dirs = paths.skills_dirs();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/home/.rho/skills"),
                PathBuf::from("/home/.agents/skills"),
                PathBuf::from("/proj/.rho/skills"),
                PathBuf::from("/proj/.agents/skills"),
            ]
        );
    }
}
