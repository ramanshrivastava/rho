//! Markdown prompt template loading and rendering.
//!
//! Port of tau's `tau_coding/prompt_templates.py` (`PromptTemplate`,
//! `load_prompt_templates`, `load_prompt_templates_with_diagnostics`,
//! `render_prompt_template`, `expand_prompt_template_command`, plus the private
//! helpers).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::resources::{
    ResourceDiagnostic, ResourceError, RhoResourcePaths, derive_description,
    parse_markdown_resource,
};

/// `{{ variable }}` placeholder regex (tau's `_TEMPLATE_VARIABLE_RE`).
static TEMPLATE_VARIABLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}").expect("valid regex"));

/// Template variables that receive invocation arguments (tau's
/// `_ARGUMENT_TEMPLATE_VARIABLES`).
const ARGUMENT_TEMPLATE_VARIABLES: [&str; 2] = ["arguments", "args"];

/// A markdown prompt template resource (tau's `PromptTemplate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    /// Template name (the markdown filename stem).
    pub name: String,
    /// Path to the markdown file.
    pub path: PathBuf,
    /// Template body (frontmatter stripped).
    pub content: String,
    /// Short description (from frontmatter or derived from the body).
    pub description: Option<String>,
}

/// Load markdown prompt templates from rho and `.agents` resource directories.
///
/// Ports tau's `load_prompt_templates`.
pub fn load_prompt_templates(
    paths: Option<&RhoResourcePaths>,
) -> Result<Vec<PromptTemplate>, ResourceError> {
    let resource_paths = paths.cloned().unwrap_or_default();

    let mut templates_by_name: BTreeMap<String, PromptTemplate> = BTreeMap::new();
    for prompts_dir in resource_paths.prompts_dirs() {
        for template in load_prompt_templates_from_dir(&prompts_dir)? {
            templates_by_name.insert(template.name.clone(), template);
        }
    }
    Ok(templates_by_name.into_values().collect())
}

/// Load prompt templates and return non-fatal discovery diagnostics.
///
/// Ports tau's `load_prompt_templates_with_diagnostics`.
#[must_use]
pub fn load_prompt_templates_with_diagnostics(
    paths: Option<&RhoResourcePaths>,
) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let resource_paths = paths.cloned().unwrap_or_default();

    let mut templates_by_name: BTreeMap<String, PromptTemplate> = BTreeMap::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    for prompts_dir in resource_paths.prompts_dirs() {
        let (templates, directory_diagnostics) =
            load_prompt_templates_from_dir_with_diagnostics(&prompts_dir);
        diagnostics.extend(directory_diagnostics);
        for template in templates {
            if let Some(previous) = templates_by_name.get(&template.name) {
                diagnostics.push(
                    ResourceDiagnostic::new(
                        "prompt",
                        format!(
                            "overrides lower-precedence resource at {}",
                            previous.path.display()
                        ),
                    )
                    .with_name(template.name.clone())
                    .with_path(template.path.clone()),
                );
            }
            templates_by_name.insert(template.name.clone(), template);
        }
    }

    (templates_by_name.into_values().collect(), diagnostics)
}

/// Render a prompt template using `{{ variable }}` placeholders.
///
/// By default (`missing` is `None`), a missing variable raises
/// [`ResourceError`]. Callers that treat templates as user-facing shortcuts can
/// pass `Some(fallback)` to render absent variables as a fallback string.
///
/// Ports tau's `render_prompt_template`.
#[allow(clippy::implicit_hasher)]
pub fn render_prompt_template(
    template: &PromptTemplate,
    variables: &HashMap<String, String>,
    missing: Option<&str>,
) -> Result<String, ResourceError> {
    let content = &template.content;
    let mut out = String::new();
    let mut last = 0;
    for captures in TEMPLATE_VARIABLE_RE.captures_iter(content) {
        let whole = captures.get(0).expect("group 0 always present");
        out.push_str(&content[last..whole.start()]);
        let name = &captures[1];
        match variables.get(name) {
            Some(value) => out.push_str(value),
            None => match missing {
                Some(fallback) => out.push_str(fallback),
                None => {
                    return Err(ResourceError(format!(
                        "Missing prompt template variable: {name}"
                    )));
                }
            },
        }
        last = whole.end();
    }
    out.push_str(&content[last..]);
    Ok(out)
}

/// Expand `/name [arguments]` text with a loaded prompt template.
///
/// Template names are matched by markdown filename stem. Invocation arguments
/// are available to templates as `{{ arguments }}` or `{{ args }}`. If a
/// template has no placeholders, arguments are appended after a blank line.
///
/// Ports tau's `expand_prompt_template_command`.
#[must_use]
pub fn expand_prompt_template_command(text: &str, templates: &[PromptTemplate]) -> Option<String> {
    let stripped = text.trim();
    if !stripped.starts_with('/') || stripped.starts_with("//") || stripped.starts_with("/skill:") {
        return None;
    }

    let (name, args) = parse_prompt_template_command(stripped);
    if name.is_empty() {
        return None;
    }

    let template = find_prompt_template(&name, templates)?;

    let mut variables: HashMap<String, String> = HashMap::new();
    variables.insert("arguments".to_string(), args.clone());
    variables.insert("args".to_string(), args.clone());
    // `missing` is `Some("")`, so rendering never fails here.
    let rendered = render_prompt_template(template, &variables, Some("")).unwrap_or_default();

    if !args.is_empty() && !template_references_arguments(&template.content) {
        return Some(format!("{}\n\n{args}", rendered.trim_end()));
    }
    Some(rendered)
}

/// Whether `content` references `{{ arguments }}` or `{{ args }}`.
fn template_references_arguments(content: &str) -> bool {
    TEMPLATE_VARIABLE_RE
        .captures_iter(content)
        .any(|captures| ARGUMENT_TEMPLATE_VARIABLES.contains(&&captures[1]))
}

/// Ports tau's `_find_prompt_template`.
fn find_prompt_template<'a>(
    name: &str,
    templates: &'a [PromptTemplate],
) -> Option<&'a PromptTemplate> {
    let normalized = name
        .trim()
        .strip_prefix('/')
        .unwrap_or(name.trim())
        .to_lowercase();
    templates
        .iter()
        .find(|template| template.name.to_lowercase() == normalized)
}

/// Ports tau's `_parse_prompt_template_command` (`text[1:].partition(" ")`).
fn parse_prompt_template_command(text: &str) -> (String, String) {
    let rest = &text[1..];
    match rest.split_once(' ') {
        Some((command, args)) => (command.trim().to_lowercase(), args.trim().to_string()),
        None => (rest.trim().to_lowercase(), String::new()),
    }
}

/// Raising variant of the per-directory loader (tau's
/// `_load_prompt_templates_from_dir`).
fn load_prompt_templates_from_dir(
    prompts_dir: &Path,
) -> Result<Vec<PromptTemplate>, ResourceError> {
    let (templates, diagnostics) = load_prompt_templates_from_dir_with_diagnostics(prompts_dir);
    if let Some(first) = diagnostics.into_iter().next() {
        return Err(ResourceError(first.message));
    }
    Ok(templates)
}

/// Ports tau's `_load_prompt_templates_from_dir_with_diagnostics`.
fn load_prompt_templates_from_dir_with_diagnostics(
    prompts_dir: &Path,
) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    if !prompts_dir.is_dir() {
        return (Vec::new(), Vec::new());
    }

    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in sorted_md_glob(prompts_dir) {
        let name = path_stem(&path);
        if seen.contains(&name) {
            diagnostics.push(
                ResourceDiagnostic::new(
                    "prompt",
                    format!(
                        "Duplicate prompt template name ignored in {}",
                        prompts_dir.display()
                    ),
                )
                .with_name(name.clone())
                .with_path(path.clone()),
            );
            continue;
        }
        seen.insert(name.clone());

        match load_prompt_template(&name, &path) {
            Ok(template) => templates.push(template),
            Err(exc) => diagnostics.push(
                ResourceDiagnostic::new("prompt", format!("could not read prompt template: {exc}"))
                    .with_name(name)
                    .with_path(path)
                    .with_severity("error"),
            ),
        }
    }

    (templates, diagnostics)
}

/// Ports tau's `_load_prompt_template`.
fn load_prompt_template(name: &str, path: &Path) -> Result<PromptTemplate, std::io::Error> {
    let raw = std::fs::read_to_string(path)?;
    let (metadata, content) = parse_markdown_resource(&raw);
    let description = metadata
        .get("description")
        .filter(|d| !d.is_empty())
        .cloned()
        .or_else(|| derive_description(&content));
    Ok(PromptTemplate {
        name: name.to_string(),
        path: path.to_path_buf(),
        content,
        description,
    })
}

/// Entries matching `*.md`, sorted by name — mirrors Python's
/// `sorted(prompts_dir.glob("*.md"), key=lambda item: item.name)`.
///
/// pathlib's `glob("*.md")` matches any entry whose name ends in `.md`
/// (case-sensitive, dotfiles included), regardless of file type.
fn sorted_md_glob(dir: &Path) -> Vec<PathBuf> {
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
        // pathlib's `glob("*.md")` is case-sensitive, so this comparison must be too.
        .filter(|(name, _)| {
            #[allow(clippy::case_sensitive_file_extension_comparisons)]
            name.ends_with(".md")
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, path)| path).collect()
}

/// Python `Path.stem`: the final component without its final suffix.
fn path_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
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

    fn template(name: &str, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            path: PathBuf::from(format!("{name}.md")),
            content: content.to_string(),
            description: None,
        }
    }

    #[test]
    fn load_prompt_templates_missing_directory_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            load_prompt_templates(Some(&paths_for(tmp.path(), None))).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn load_prompt_templates_from_markdown_files() {
        let tmp = tempfile::tempdir().unwrap();
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("review.md"),
            "---\ndescription: Review code\n---\nReview {{ topic }}.",
        )
        .unwrap();

        let templates = load_prompt_templates(Some(&paths_for(tmp.path(), None))).unwrap();

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "review");
        assert_eq!(templates[0].description.as_deref(), Some("Review code"));
    }

    #[test]
    fn load_prompt_templates_includes_agents_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(agents_home.join("prompts")).unwrap();
        std::fs::write(agents_home.join("prompts").join("user.md"), "User prompt").unwrap();
        std::fs::create_dir_all(cwd.join(".agents").join("prompts")).unwrap();
        std::fs::write(
            cwd.join(".agents").join("prompts").join("project.md"),
            "Project prompt",
        )
        .unwrap();

        let templates =
            load_prompt_templates(Some(&paths_for_cwd(&rho_home, Some(&agents_home), &cwd)))
                .unwrap();

        assert_eq!(
            templates
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["project", "user"]
        );
    }

    #[test]
    fn project_prompt_template_overrides_user_template() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(agents_home.join("prompts")).unwrap();
        std::fs::write(agents_home.join("prompts").join("review.md"), "User review").unwrap();
        std::fs::create_dir_all(cwd.join(".agents").join("prompts")).unwrap();
        std::fs::write(
            cwd.join(".agents").join("prompts").join("review.md"),
            "Project review",
        )
        .unwrap();

        let templates =
            load_prompt_templates(Some(&paths_for_cwd(&rho_home, Some(&agents_home), &cwd)))
                .unwrap();

        assert_eq!(templates.len(), 1);
        assert_eq!(
            templates[0].path,
            cwd.join(".agents").join("prompts").join("review.md")
        );
        assert_eq!(templates[0].content, "Project review");
    }

    #[test]
    fn load_prompt_templates_with_diagnostics_reports_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let rho_home = tmp.path().join("home").join(".rho");
        let agents_home = tmp.path().join("home").join(".agents");
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(rho_home.join("prompts")).unwrap();
        std::fs::write(
            rho_home.join("prompts").join("review.md"),
            "User Rho review",
        )
        .unwrap();
        std::fs::create_dir_all(cwd.join(".rho").join("prompts")).unwrap();
        std::fs::write(
            cwd.join(".rho").join("prompts").join("review.md"),
            "Project Rho review",
        )
        .unwrap();

        let (templates, diagnostics) = load_prompt_templates_with_diagnostics(Some(
            &paths_for_cwd(&rho_home, Some(&agents_home), &cwd),
        ));

        assert_eq!(
            templates
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["review"]
        );
        assert_eq!(
            templates[0].path,
            cwd.join(".rho").join("prompts").join("review.md")
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, "prompt");
        assert_eq!(diagnostics[0].name.as_deref(), Some("review"));
        assert!(
            diagnostics[0]
                .message
                .contains("overrides lower-precedence resource")
        );
    }

    #[test]
    fn render_prompt_template_replaces_variables() {
        let tmpl = template("review", "Review {{ topic }} for {{ focus }}.");
        let mut vars = HashMap::new();
        vars.insert("topic".to_string(), "auth".to_string());
        vars.insert("focus".to_string(), "security".to_string());
        assert_eq!(
            render_prompt_template(&tmpl, &vars, None).unwrap(),
            "Review auth for security."
        );
    }

    #[test]
    fn render_prompt_template_rejects_missing_variables() {
        let tmpl = template("review", "Review {{ topic }}.");
        let err = render_prompt_template(&tmpl, &HashMap::new(), None).unwrap_err();
        assert!(err.to_string().contains("Missing prompt template variable"));
    }

    #[test]
    fn expand_prompt_template_command_replaces_slash_command() {
        let tmpl = template("example", "Use these arguments: {{ arguments }}");
        assert_eq!(
            expand_prompt_template_command("/example src/app.py", &[tmpl]),
            Some("Use these arguments: src/app.py".to_string())
        );
    }

    #[test]
    fn expand_prompt_template_command_blanks_missing_custom_variables() {
        let tmpl = template(
            "review",
            "Base branch: {{ base_branch }}\nReview PR {{ arguments }}.",
        );
        assert_eq!(
            expand_prompt_template_command("/review 168", &[tmpl]),
            Some("Base branch: \nReview PR 168.".to_string())
        );
    }

    #[test]
    fn expand_prompt_template_command_appends_arguments_without_argument_placeholder() {
        let tmpl = template(
            "review",
            "Base branch: {{ base_branch }}\nReview this code.",
        );
        assert_eq!(
            expand_prompt_template_command("/review src/app.py", &[tmpl]),
            Some("Base branch: \nReview this code.\n\nsrc/app.py".to_string())
        );
    }

    #[test]
    fn expand_prompt_template_command_appends_arguments_without_placeholder() {
        let tmpl = template("review", "Review this code.");
        assert_eq!(
            expand_prompt_template_command("/review src/app.py", &[tmpl]),
            Some("Review this code.\n\nsrc/app.py".to_string())
        );
    }

    #[test]
    fn expand_prompt_template_command_ignores_unknown_commands() {
        let tmpl = template("review", "Review this code.");
        assert_eq!(
            expand_prompt_template_command("/missing src/app.py", &[tmpl]),
            None
        );
    }
}
