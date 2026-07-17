//! Markdown skill loading and expansion.
//!
//! Port of tau's `tau_coding/skills.py` (`Skill`, `SkillInvocation`,
//! `load_skills`, `load_skills_with_diagnostics`, `expand_skill_command`,
//! `format_skill_invocation`, `parse_skill_invocation`, `build_skill_index`,
//! and the private `_load_skills_from_dir*` / `_load_skill` helpers).
//!
//! Follows the Agent Skills spec: a skill is a directory containing a
//! `SKILL.md` file. Bare `*.md` files at the root of a skills directory are not
//! skills; they surface as informational diagnostics (ADR 0003).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::resources::{
    ResourceDiagnostic, ResourceError, RhoResourcePaths, derive_description,
    parse_markdown_resource,
};

/// Tau's expanded skill-invocation format regex.
///
/// Ported verbatim from tau's
/// `^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n</skill>(?:\n\n([\s\S]+))?$`.
/// A trailing `\n?` is added before the final `$` so the anchor matches at the
/// end of the haystack *or* just before a single trailing newline — Python's
/// `re.match` `$` semantics, which the Rust `regex` crate does not provide by
/// default.
static SKILL_INVOCATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n</skill>(?:\n\n([\s\S]+))?\n?$"#,
    )
    .expect("skill invocation regex is valid")
});

/// A markdown skill resource.
///
/// Ports tau's `Skill` dataclass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill name (the containing directory name).
    pub name: String,
    /// Path to the `SKILL.md` file.
    pub path: PathBuf,
    /// Skill body (frontmatter stripped).
    pub content: String,
    /// Short description (from frontmatter or derived from the body).
    pub description: Option<String>,
}

/// Parsed expanded skill-invocation message.
///
/// Ports tau's `SkillInvocation` dataclass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocation {
    /// Skill name.
    pub name: String,
    /// Skill location (the `SKILL.md` path, as a string).
    pub location: String,
    /// Skill body.
    pub content: String,
    /// Optional trailing user instructions.
    pub additional_instructions: Option<String>,
}

/// Load markdown skills from rho and `.agents` resource directories.
///
/// Resource directories are loaded in increasing precedence order, so project
/// resources override user resources with the same skill name. Duplicate names
/// within the same directory remain invalid (they raise).
///
/// Ports tau's `load_skills`.
pub fn load_skills(paths: Option<&RhoResourcePaths>) -> Result<Vec<Skill>, ResourceError> {
    let resource_paths = paths.cloned().unwrap_or_default();

    let mut skills_by_name: BTreeMap<String, Skill> = BTreeMap::new();
    for skills_dir in resource_paths.skills_dirs() {
        for skill in load_skills_from_dir(&skills_dir)? {
            skills_by_name.insert(skill.name.clone(), skill);
        }
    }
    Ok(skills_by_name.into_values().collect())
}

/// Load skills and return non-fatal discovery diagnostics.
///
/// Resource directories are loaded in increasing precedence order. Higher
/// precedence resources replace lower precedence resources with the same name,
/// and that replacement is reported as a diagnostic.
///
/// Ports tau's `load_skills_with_diagnostics`.
#[must_use]
pub fn load_skills_with_diagnostics(
    paths: Option<&RhoResourcePaths>,
) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
    let resource_paths = paths.cloned().unwrap_or_default();

    let mut skills_by_name: BTreeMap<String, Skill> = BTreeMap::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    for skills_dir in resource_paths.skills_dirs() {
        let (skills, directory_diagnostics) = load_skills_from_dir_with_diagnostics(&skills_dir);
        diagnostics.extend(directory_diagnostics);
        for skill in skills {
            if let Some(previous) = skills_by_name.get(&skill.name) {
                diagnostics.push(
                    ResourceDiagnostic::new(
                        "skill",
                        format!(
                            "overrides lower-precedence resource at {}",
                            previous.path.display()
                        ),
                    )
                    .with_name(skill.name.clone())
                    .with_path(skill.path.clone()),
                );
            }
            skills_by_name.insert(skill.name.clone(), skill);
        }
    }

    (skills_by_name.into_values().collect(), diagnostics)
}

/// Expand `/skill:name` prompt text, or return `None` for non-skill text.
///
/// Ports tau's `expand_skill_command`.
pub fn expand_skill_command(text: &str, skills: &[Skill]) -> Result<Option<String>, ResourceError> {
    let stripped = text.trim();
    if !stripped.starts_with("/skill:") {
        return Ok(None);
    }

    // Python `str.partition(" ")`: split on the first space.
    let (command, separator, request) = match stripped.split_once(' ') {
        Some((command, request)) => (command, true, request),
        None => (stripped, false, ""),
    };
    let name = command.strip_prefix("/skill:").unwrap_or(command).trim();
    if name.is_empty() {
        return Err(ResourceError(
            "Skill command must include a skill name".to_string(),
        ));
    }

    let skill = skills.iter().find(|skill| skill.name == name);
    let Some(skill) = skill else {
        return Err(ResourceError(format!("Unknown skill: {name}")));
    };

    let additional_instructions = if separator {
        Some(request.trim().to_string())
    } else {
        None
    };
    Ok(Some(format_skill_invocation(
        skill,
        additional_instructions.as_deref(),
    )))
}

/// Format a full skill-invocation prompt.
///
/// Byte-sensitive: this string is round-tripped by [`parse_skill_invocation`].
/// Ports tau's `format_skill_invocation`.
#[must_use]
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.path.display(),
        python_parent(&skill.path),
        skill.content.trim(),
    );
    match additional_instructions {
        Some(extra) if !extra.trim().is_empty() => {
            format!("{skill_block}\n\n{}", extra.trim())
        }
        _ => skill_block,
    }
}

/// Parse rho's expanded skill-invocation message format.
///
/// Ports tau's `parse_skill_invocation`.
#[must_use]
pub fn parse_skill_invocation(text: &str) -> Option<SkillInvocation> {
    let captures = SKILL_INVOCATION_RE.captures(text)?;
    Some(SkillInvocation {
        name: captures[1].to_string(),
        location: captures[2].to_string(),
        content: captures[3].to_string(),
        additional_instructions: captures.get(4).map(|m| m.as_str().to_string()),
    })
}

/// Build a concise index of available skills (tau's `build_skill_index`).
#[must_use]
pub fn build_skill_index(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "Available skills: none".to_string();
    }
    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut lines = vec!["Available skills:".to_string()];
    for skill in sorted {
        let description = skill
            .description
            .as_deref()
            .filter(|d| !d.is_empty())
            .unwrap_or("No description");
        lines.push(format!("- {}: {description}", skill.name));
    }
    lines.join("\n")
}

/// Raising variant of the per-directory loader (tau's `_load_skills_from_dir`).
///
/// Bare-`.md` migration hints are informational — the file is skipped, but that
/// is not an error. Only fatal problems raise here; the full diagnostic stream
/// is available through [`load_skills_with_diagnostics`].
fn load_skills_from_dir(skills_dir: &Path) -> Result<Vec<Skill>, ResourceError> {
    let (skills, diagnostics) = load_skills_from_dir_with_diagnostics(skills_dir);
    for diagnostic in diagnostics {
        if diagnostic.severity == "info" {
            continue;
        }
        return Err(ResourceError(diagnostic.message));
    }
    Ok(skills)
}

/// Ports tau's `_load_skills_from_dir_with_diagnostics`.
fn load_skills_from_dir_with_diagnostics(
    skills_dir: &Path,
) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
    if !skills_dir.is_dir() {
        return (Vec::new(), Vec::new());
    }

    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in sorted_dir_entries(skills_dir) {
        let skill_path: PathBuf;
        let name: String;
        if path.is_dir() {
            skill_path = path.join("SKILL.md");
            name = path_name(&path);
            if !skill_path.exists() {
                continue;
            }
        } else if path.is_file() && has_md_suffix(&path) {
            if path_name(&path).to_uppercase() == "AGENTS.MD" {
                continue;
            }
            let stem = path_stem(&path);
            let target = skills_dir.join(&stem).join("SKILL.md");
            diagnostics.push(
                ResourceDiagnostic::new(
                    "skill",
                    format!(
                        "bare .md files are no longer treated as skills; move it to {}",
                        target.display()
                    ),
                )
                .with_name(stem)
                .with_path(path.clone())
                .with_severity("info"),
            );
            continue;
        } else {
            continue;
        }

        if seen.contains(&name) {
            diagnostics.push(
                ResourceDiagnostic::new(
                    "skill",
                    format!("Duplicate skill name ignored in {}", skills_dir.display()),
                )
                .with_name(name.clone())
                .with_path(skill_path.clone()),
            );
            continue;
        }
        seen.insert(name.clone());

        match load_skill(&name, &skill_path) {
            Ok(skill) => skills.push(skill),
            Err(exc) => diagnostics.push(
                ResourceDiagnostic::new("skill", format!("could not read skill: {exc}"))
                    .with_name(name)
                    .with_path(skill_path)
                    .with_severity("error"),
            ),
        }
    }

    (skills, diagnostics)
}

/// Ports tau's `_load_skill`.
fn load_skill(name: &str, path: &Path) -> Result<Skill, std::io::Error> {
    let raw = std::fs::read_to_string(path)?;
    let (metadata, content) = parse_markdown_resource(&raw);
    let description = metadata
        .get("description")
        .filter(|d| !d.is_empty())
        .cloned()
        .or_else(|| derive_description(&content));
    Ok(Skill {
        name: name.to_string(),
        path: path.to_path_buf(),
        content,
        description,
    })
}

/// Sorted directory entries (by final component name), matching Python's
/// `sorted(path.iterdir(), key=lambda item: item.name)`.
fn sorted_dir_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, PathBuf)> = read_dir
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, path)| path).collect()
}

/// Python `Path.name`: the final path component.
fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Python `Path.stem`: the final component without its final suffix.
fn path_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Python `Path.suffix.lower() == ".md"`: case-insensitive `.md` extension.
fn has_md_suffix(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.to_string_lossy().to_lowercase() == "md")
}

/// Python `str(Path.parent)`: the parent directory as a string.
///
/// Matches pathlib semantics: a bare relative name yields `"."`, and the
/// filesystem root yields itself.
fn python_parent(path: &Path) -> String {
    match path.parent() {
        Some(parent) => {
            let text = parent.to_string_lossy();
            if text.is_empty() {
                ".".to_string()
            } else {
                text.into_owned()
            }
        }
        None => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_for(root: &Path, agents_root: Option<&Path>) -> RhoResourcePaths {
        RhoResourcePaths {
            root: root.to_path_buf(),
            cwd: None,
            agents_root: agents_root.map(Path::to_path_buf),
            paths: None,
        }
    }

    fn paths_for_cwd(root: &Path, agents_root: Option<&Path>, cwd: &Path) -> RhoResourcePaths {
        RhoResourcePaths {
            root: root.to_path_buf(),
            cwd: Some(cwd.to_path_buf()),
            agents_root: agents_root.map(Path::to_path_buf),
            paths: None,
        }
    }

    #[test]
    fn load_skills_missing_directory_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();
        assert_eq!(skills, Vec::new());
    }

    #[test]
    fn load_skills_from_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("python-testing")).unwrap();
        std::fs::write(
            skills_dir.join("python-testing").join("SKILL.md"),
            "---\ndescription: Test Python code\n---\n# Python Testing\nUse pytest.",
        )
        .unwrap();
        std::fs::create_dir_all(skills_dir.join("git-review")).unwrap();
        std::fs::write(
            skills_dir.join("git-review").join("SKILL.md"),
            "# Git Review\nReview diffs.",
        )
        .unwrap();

        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["git-review", "python-testing"]
        );
        assert_eq!(skills[0].description.as_deref(), Some("Git Review"));
        assert_eq!(skills[1].description.as_deref(), Some("Test Python code"));
    }

    #[test]
    fn load_skills_includes_user_and_project_agents_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(agents_home.join("skills").join("user-skill")).unwrap();
        std::fs::write(
            agents_home
                .join("skills")
                .join("user-skill")
                .join("SKILL.md"),
            "# User Skill\nFrom user agents.",
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".agents").join("skills").join("project-skill")).unwrap();
        std::fs::write(
            cwd.join(".agents")
                .join("skills")
                .join("project-skill")
                .join("SKILL.md"),
            "# Project Skill\nFrom project agents.",
        )
        .unwrap();

        let skills =
            load_skills(Some(&paths_for_cwd(&rho_home, Some(&agents_home), &cwd))).unwrap();

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["project-skill", "user-skill"]
        );
    }

    #[test]
    fn project_agents_skill_overrides_user_agents_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(agents_home.join("skills").join("review")).unwrap();
        std::fs::write(
            agents_home.join("skills").join("review").join("SKILL.md"),
            "# User Review",
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".agents").join("skills").join("review")).unwrap();
        std::fs::write(
            cwd.join(".agents")
                .join("skills")
                .join("review")
                .join("SKILL.md"),
            "# Project Review",
        )
        .unwrap();

        let skills =
            load_skills(Some(&paths_for_cwd(&rho_home, Some(&agents_home), &cwd))).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].path,
            cwd.join(".agents")
                .join("skills")
                .join("review")
                .join("SKILL.md")
        );
        assert_eq!(skills[0].description.as_deref(), Some("Project Review"));
    }

    #[test]
    fn load_skills_with_diagnostics_reports_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(rho_home.join("skills").join("review")).unwrap();
        std::fs::write(
            rho_home.join("skills").join("review").join("SKILL.md"),
            "# User Rho Review",
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".rho").join("skills").join("review")).unwrap();
        std::fs::write(
            cwd.join(".rho")
                .join("skills")
                .join("review")
                .join("SKILL.md"),
            "# Project Rho Review",
        )
        .unwrap();

        let (skills, diagnostics) =
            load_skills_with_diagnostics(Some(&paths_for_cwd(&rho_home, Some(&agents_home), &cwd)));

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["review"]
        );
        assert_eq!(
            skills[0].path,
            cwd.join(".rho")
                .join("skills")
                .join("review")
                .join("SKILL.md")
        );
        let override_diagnostics: Vec<&ResourceDiagnostic> = diagnostics
            .iter()
            .filter(|d| d.message.contains("overrides lower-precedence resource"))
            .collect();
        assert_eq!(override_diagnostics.len(), 1);
        assert_eq!(override_diagnostics[0].kind, "skill");
        assert_eq!(override_diagnostics[0].name.as_deref(), Some("review"));
    }

    #[test]
    fn load_skills_with_diagnostics_reports_bare_md_migration_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("legacy.md"), "# Legacy Skill\nOld body.").unwrap();
        std::fs::create_dir_all(skills_dir.join("good")).unwrap();
        std::fs::write(skills_dir.join("good").join("SKILL.md"), "# Good Skill").unwrap();

        let (skills, diagnostics) =
            load_skills_with_diagnostics(Some(&paths_for(tmp.path(), None)));

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["good"]
        );
        let migration: Vec<&ResourceDiagnostic> = diagnostics
            .iter()
            .filter(|d| d.severity == "info")
            .collect();
        assert_eq!(migration.len(), 1);
        assert_eq!(migration[0].name.as_deref(), Some("legacy"));
        assert_eq!(
            migration[0].path.as_deref(),
            Some(skills_dir.join("legacy.md").as_path())
        );
        assert!(
            migration[0]
                .message
                .contains("bare .md files are no longer treated as skills")
        );
        assert!(
            migration[0].message.contains(
                &skills_dir
                    .join("legacy")
                    .join("SKILL.md")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn agents_root_is_not_a_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_home = tmp.path().join(".agents");
        std::fs::create_dir_all(&agents_home).unwrap();
        std::fs::write(agents_home.join("AGENTS.md"), "# Instructions").unwrap();
        std::fs::write(agents_home.join("README.md"), "# Readme").unwrap();
        std::fs::write(agents_home.join("review.md"), "# Review").unwrap();

        let skills = load_skills(Some(&paths_for(
            &tmp.path().join(".rho"),
            Some(&agents_home),
        )))
        .unwrap();

        assert_eq!(skills, Vec::new());
    }

    #[test]
    fn agents_skills_dir_ignores_bare_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_home = tmp.path().join(".agents");
        let skills_dir = agents_home.join("skills");
        std::fs::create_dir_all(skills_dir.join("my-skill")).unwrap();
        std::fs::write(
            skills_dir.join("my-skill").join("SKILL.md"),
            "# Valid Skill",
        )
        .unwrap();
        std::fs::write(skills_dir.join("reference.md"), "# Reference doc").unwrap();

        let skills = load_skills(Some(&paths_for(
            &tmp.path().join(".rho"),
            Some(&agents_home),
        )))
        .unwrap();

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["my-skill"]
        );
    }

    #[test]
    fn rho_skills_dir_ignores_bare_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("my-skill")).unwrap();
        std::fs::write(
            skills_dir.join("my-skill").join("SKILL.md"),
            "# Subdir Skill",
        )
        .unwrap();
        std::fs::write(skills_dir.join("reference.md"), "# Reference doc").unwrap();

        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["my-skill"]
        );
    }

    #[test]
    fn expand_skill_command_includes_skill_and_user_request() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills").join("testing");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("SKILL.md"), "# Testing\nRun pytest.").unwrap();
        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();

        let expanded = expand_skill_command("/skill:testing add parser tests", &skills)
            .unwrap()
            .unwrap();

        assert!(expanded.contains(&format!(
            "<skill name=\"testing\" location=\"{}\">",
            skills[0].path.display()
        )));
        assert!(expanded.contains(&format!(
            "References are relative to {}.",
            python_parent(&skills[0].path)
        )));
        assert!(expanded.contains("Run pytest."));
        assert!(expanded.ends_with("</skill>\n\nadd parser tests"));
    }

    #[test]
    fn format_skill_invocation_without_extra_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills").join("testing").join("SKILL.md");
        let skill = Skill {
            name: "testing".to_string(),
            path: path.clone(),
            content: "# Testing\nRun pytest.".to_string(),
            description: Some("Test code".to_string()),
        };

        let formatted = format_skill_invocation(&skill, None);

        assert_eq!(
            formatted,
            format!(
                "<skill name=\"testing\" location=\"{}\">\nReferences are relative to {}.\n\n# Testing\nRun pytest.\n</skill>",
                path.display(),
                python_parent(&path),
            )
        );
    }

    #[test]
    fn parse_skill_invocation_extracts_display_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills").join("testing").join("SKILL.md");
        let skill = Skill {
            name: "testing".to_string(),
            path: path.clone(),
            content: "# Testing\nRun pytest.".to_string(),
            description: Some("Test Python code".to_string()),
        };
        let formatted = format_skill_invocation(&skill, Some("add parser tests"));

        let parsed = parse_skill_invocation(&formatted).unwrap();

        assert_eq!(parsed.name, "testing");
        assert_eq!(parsed.location, path.display().to_string());
        assert!(parsed.content.contains("# Testing"));
        assert_eq!(
            parsed.additional_instructions.as_deref(),
            Some("add parser tests")
        );
    }

    #[test]
    fn parse_skill_invocation_round_trips_with_trailing_newline() {
        // Python's `re.match` `$` matches before a single trailing newline.
        let skill = Skill {
            name: "testing".to_string(),
            path: PathBuf::from("/skills/testing/SKILL.md"),
            content: "# Testing".to_string(),
            description: None,
        };
        let formatted = format_skill_invocation(&skill, None);
        let with_newline = format!("{formatted}\n");
        let parsed = parse_skill_invocation(&with_newline).unwrap();
        assert_eq!(parsed.name, "testing");
        assert_eq!(parsed.additional_instructions, None);
    }

    #[test]
    fn expand_skill_command_returns_none_for_normal_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();
        assert_eq!(expand_skill_command("hello", &skills).unwrap(), None);
    }

    #[test]
    fn expand_skill_command_rejects_unknown_skill() {
        let err = expand_skill_command("/skill:missing", &[]).unwrap_err();
        assert!(err.to_string().contains("Unknown skill"));
    }

    #[test]
    fn expand_skill_command_rejects_empty_name() {
        let err = expand_skill_command("/skill: ", &[]).unwrap_err();
        assert!(err.to_string().contains("must include a skill name"));
    }

    #[test]
    fn build_skill_index_renders_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills").join("testing");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\ndescription: Test things\n---\nBody",
        )
        .unwrap();

        let skills = load_skills(Some(&paths_for(tmp.path(), None))).unwrap();
        assert_eq!(
            build_skill_index(&skills),
            "Available skills:\n- testing: Test things"
        );
    }

    #[test]
    fn build_skill_index_empty() {
        assert_eq!(build_skill_index(&[]), "Available skills: none");
    }
}
