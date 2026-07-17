//! Project instruction discovery for rho coding sessions.
//!
//! Port of tau's `tau_coding/context.py`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::paths::resolve_path;
use crate::resources::{ResourceDiagnostic, RhoResourcePaths};
use crate::system_prompt::ProjectContextFile;

/// Filesystem markers that identify a project root.
pub const PROJECT_MARKERS: [&str; 5] = [
    ".git",
    "pyproject.toml",
    "uv.lock",
    "setup.py",
    "package.json",
];

/// Discover project instruction files for system prompt context.
#[must_use]
pub fn discover_project_context(paths: Option<RhoResourcePaths>) -> Vec<ProjectContextFile> {
    discover_project_context_with_diagnostics(paths).0
}

/// Discover project instruction files and return non-fatal diagnostics.
#[must_use]
pub fn discover_project_context_with_diagnostics(
    paths: Option<RhoResourcePaths>,
) -> (Vec<ProjectContextFile>, Vec<ResourceDiagnostic>) {
    let resource_paths = paths.unwrap_or_default();
    let mut context_files: Vec<ProjectContextFile> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    for path in context_file_candidates(&resource_paths) {
        match std::fs::read_to_string(&path) {
            Ok(content) => context_files.push(ProjectContextFile {
                path: path.to_string_lossy().into_owned(),
                content,
            }),
            Err(err) => diagnostics.push(
                ResourceDiagnostic::new("context", format!("could not read context file: {err}"))
                    .with_path(path),
            ),
        }
    }
    (context_files, diagnostics)
}

fn context_file_candidates(paths: &RhoResourcePaths) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![paths.root.join("AGENTS.md")];
    if let Some(agents_root) = &paths.agents_root {
        candidates.push(agents_root.join("AGENTS.md"));
    }

    if let Some(cwd) = &paths.cwd {
        let cwd = resolve_path(&expanduser(cwd));
        let project_root = find_project_root(&cwd);
        candidates.extend(ancestor_agents_files(&project_root, &cwd));
        let rho_paths = paths.resolved_paths();
        candidates.push(rho_paths.project_rho_dir(&cwd).join("AGENTS.md"));
        candidates.push(rho_paths.project_agents_dir(&cwd).join("AGENTS.md"));
    }

    let existing: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    dedupe_resolved_paths(existing)
}

fn find_project_root(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if PROJECT_MARKERS
            .iter()
            .any(|marker| ancestor.join(marker).exists())
        {
            return ancestor.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

fn ancestor_agents_files(project_root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let Ok(relative) = cwd.strip_prefix(project_root) else {
        return vec![cwd.join("AGENTS.md")];
    };
    let mut paths = vec![project_root.join("AGENTS.md")];
    let mut current = project_root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            current = current.join(part);
            paths.push(current.join("AGENTS.md"));
        }
    }
    paths
}

fn dedupe_resolved_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut deduped: Vec<PathBuf> = Vec::new();
    for path in paths {
        let resolved = resolve_path(&expanduser(&path));
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
        return crate::paths::home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return crate::paths::home_dir().join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RhoPaths;

    #[test]
    fn discovers_user_project_and_agents_context_files() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonicalize the temp base so expected paths match the resolver's
        // output regardless of platform symlinks (e.g. macOS `/private`).
        let base = tmp.path().canonicalize().unwrap();

        let rho_home = base.join("home").join(".rho");
        let agents_home = base.join("home").join(".agents");
        let project = base.join("project");
        let nested = project.join("pkg");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project.join("pyproject.toml"), "[project]\nname = 'demo'\n").unwrap();
        std::fs::create_dir_all(&rho_home).unwrap();
        std::fs::create_dir_all(&agents_home).unwrap();
        std::fs::create_dir(project.join(".rho")).unwrap();
        std::fs::create_dir(project.join(".agents")).unwrap();

        std::fs::write(rho_home.join("AGENTS.md"), "User rho instructions").unwrap();
        std::fs::write(agents_home.join("AGENTS.md"), "User agents instructions").unwrap();
        std::fs::write(project.join("AGENTS.md"), "Project instructions").unwrap();
        std::fs::write(nested.join("AGENTS.md"), "Nested instructions").unwrap();
        std::fs::create_dir(nested.join(".rho")).unwrap();
        std::fs::create_dir(nested.join(".agents")).unwrap();
        std::fs::write(
            nested.join(".rho").join("AGENTS.md"),
            "Project rho instructions",
        )
        .unwrap();
        std::fs::write(
            nested.join(".agents").join("AGENTS.md"),
            "Project agents instructions",
        )
        .unwrap();

        let resource_paths = RhoResourcePaths {
            root: rho_home.clone(),
            cwd: Some(nested.clone()),
            agents_root: Some(agents_home.clone()),
            paths: Some(RhoPaths::new(rho_home.clone(), agents_home.clone())),
        };
        let context_files = discover_project_context(Some(resource_paths));

        let paths: Vec<PathBuf> = context_files
            .iter()
            .map(|file| PathBuf::from(&file.path))
            .collect();
        assert_eq!(
            paths,
            vec![
                rho_home.join("AGENTS.md"),
                agents_home.join("AGENTS.md"),
                project.join("AGENTS.md"),
                nested.join("AGENTS.md"),
                nested.join(".rho").join("AGENTS.md"),
                nested.join(".agents").join("AGENTS.md"),
            ]
        );
        let contents: Vec<&str> = context_files
            .iter()
            .map(|file| file.content.as_str())
            .collect();
        assert_eq!(
            contents,
            vec![
                "User rho instructions",
                "User agents instructions",
                "Project instructions",
                "Nested instructions",
                "Project rho instructions",
                "Project agents instructions",
            ]
        );
    }
}
