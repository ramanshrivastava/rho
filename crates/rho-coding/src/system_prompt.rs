//! System prompt assembly for rho coding sessions (port of tau's
//! `tau_coding/system_prompt.py`). Byte-identical to tau for a fixed tool set,
//! date, and cwd — pinned by the `fixtures/system-prompt/` golden.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rho_agent::tools::AgentTool;

/// A calendar date (tau uses `datetime.date`); rendered ISO-8601.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// Year.
    pub year: i64,
    /// Month (1-12).
    pub month: u32,
    /// Day (1-31).
    pub day: u32,
}

impl Date {
    /// Build a date.
    #[must_use]
    pub fn new(year: i64, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// ISO-8601 `YYYY-MM-DD` (tau `date.isoformat()`).
    #[must_use]
    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Today in UTC (tau `date.today()` — local time in tau; rho uses UTC, which
    /// only shifts the production-mode date near midnight and never affects a
    /// golden, which always pins `current_date`).
    #[must_use]
    pub fn today() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self::from_unix_days(i64::try_from(secs / 86_400).unwrap_or(0))
    }

    /// Civil date from days-since-epoch (Howard Hinnant's algorithm).
    fn from_unix_days(z: i64) -> Self {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        Self {
            year: if m <= 2 { y + 1 } else { y },
            month: u32::try_from(m).unwrap_or(1),
            day: u32::try_from(d).unwrap_or(1),
        }
    }
}

/// A project instruction file included in the prompt (tau `ProjectContextFile`).
#[derive(Debug, Clone)]
pub struct ProjectContextFile {
    /// The file's display path.
    pub path: String,
    /// The file's content.
    pub content: String,
}

/// A discovered skill (minimal M4a slice of tau's `tau_coding.skills.Skill` —
/// only the fields the system prompt renders).
#[derive(Debug, Clone)]
pub struct Skill {
    /// Skill name.
    pub name: String,
    /// Path to the skill file.
    pub path: PathBuf,
    /// Short description.
    pub description: String,
}

/// Options for [`build_system_prompt`] (tau `BuildSystemPromptOptions`).
#[derive(Debug, Clone, Default)]
pub struct BuildSystemPromptOptions {
    /// Working directory.
    pub cwd: PathBuf,
    /// Visible tools.
    pub tools: Vec<AgentTool>,
    /// Available skills.
    pub skills: Vec<Skill>,
    /// A custom base prompt replacing the default preamble.
    pub custom_prompt: Option<String>,
    /// Extra text appended after the base prompt.
    pub append_system_prompt: Option<String>,
    /// Project context files.
    pub context_files: Vec<ProjectContextFile>,
    /// The current date (defaults to today).
    pub current_date: Option<Date>,
    /// Extra guidelines to fold in.
    pub extra_guidelines: Vec<String>,
}

/// Build a deterministic Pi-style system prompt (tau `build_system_prompt`).
#[must_use]
pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let current_date = options.current_date.unwrap_or_else(Date::today);
    let cwd = format_path(&options.cwd);
    let append_section = options
        .append_system_prompt
        .as_ref()
        .map_or_else(String::new, |a| format!("\n\n{a}"));

    let mut prompt = if let Some(custom) = &options.custom_prompt {
        custom.clone()
    } else {
        format!(
            "You are an expert coding assistant operating inside Tau, a coding agent harness. \
You help users by reading files, executing commands, editing code, and writing new files.\
\n\nAvailable tools:\n{}\
\n\nIn addition to the tools above, you may have access to other custom tools depending on the \
project.\
\n\nGuidelines:\n{}",
            format_available_tools(&options.tools),
            format_guidelines(&options.tools, &options.extra_guidelines),
        )
    };

    prompt.push_str(&append_section);
    prompt.push_str(&format_project_context(&options.context_files));
    if has_tool(&options.tools, "read") {
        prompt.push_str(&format_skills_for_prompt(&options.skills));
    }
    let _ = write!(prompt, "\nCurrent date: {}", current_date.iso());
    let _ = write!(prompt, "\nCurrent working directory: {cwd}");
    prompt
}

/// Format visible tools using prompt snippets (tau `format_available_tools`).
#[must_use]
pub fn format_available_tools(tools: &[AgentTool]) -> String {
    let lines: Vec<String> = tools
        .iter()
        .filter_map(|t| {
            t.prompt_snippet
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!("- {}: {s}", t.name))
        })
        .collect();
    if lines.is_empty() {
        "(none)".to_string()
    } else {
        lines.join("\n")
    }
}

/// Collect and de-duplicate guidelines (tau `collect_prompt_guidelines`).
#[must_use]
pub fn collect_prompt_guidelines(tools: &[AgentTool], extra_guidelines: &[String]) -> Vec<String> {
    let names: std::collections::HashSet<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let mut guidelines: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut add = |value: &str| {
        let normalized = value.trim().to_string();
        if normalized.is_empty() || seen.contains(&normalized) {
            return;
        }
        seen.insert(normalized.clone());
        guidelines.push(normalized);
    };

    let has_bash = names.contains("bash");
    let has_exploration = names.contains("grep") || names.contains("find") || names.contains("ls");
    if has_bash && !has_exploration {
        add("Use bash for file operations like ls, rg, find");
    } else if has_bash && has_exploration {
        add(
            "Prefer grep/find/ls tools over bash for file exploration (faster, respects .gitignore)",
        );
    }

    for tool in tools {
        for guideline in &tool.prompt_guidelines {
            add(guideline);
        }
    }
    for guideline in extra_guidelines {
        add(guideline);
    }

    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    guidelines
}

/// Format guidelines as markdown bullets (tau `format_guidelines`).
#[must_use]
pub fn format_guidelines(tools: &[AgentTool], extra_guidelines: &[String]) -> String {
    collect_prompt_guidelines(tools, extra_guidelines)
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format project context files (tau `format_project_context`).
#[must_use]
pub fn format_project_context(context_files: &[ProjectContextFile]) -> String {
    if context_files.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "\n\n<project_context>".into(),
        String::new(),
        "Project-specific instructions and guidelines:".into(),
        String::new(),
    ];
    for context_file in context_files {
        lines.push(format!(
            "<project_instructions path=\"{}\">",
            xml_escape(&context_file.path)
        ));
        lines.push(context_file.content.clone());
        lines.push("</project_instructions>".into());
        lines.push(String::new());
    }
    lines.push("</project_context>".into());
    lines.join("\n")
}

/// Format skills as Pi-style XML (tau `format_skills_for_prompt`).
#[must_use]
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".into(),
        "Read the full skill file when the task matches its description.".into(),
        "When a skill file references a relative path, resolve it against the skill directory \
(parent of SKILL.md / dirname of the path) and use that absolute path in tool commands."
            .into(),
        String::new(),
        "<available_skills>".into(),
    ];
    let mut sorted: Vec<&Skill> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for skill in sorted {
        let description = if skill.description.is_empty() {
            "No description"
        } else {
            &skill.description
        };
        lines.push("  <skill>".into());
        lines.push(format!("    <name>{}</name>", xml_escape(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            xml_escape(description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            xml_escape(&skill.path.display().to_string())
        ));
        lines.push("  </skill>".into());
    }
    lines.push("</available_skills>".into());
    lines.join("\n")
}

fn has_tool(tools: &[AgentTool], name: &str) -> bool {
    tools.iter().any(|t| t.name == name)
}

fn format_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Escape `&`, `<`, `>` (Python `xml.sax.saxutils.escape` defaults).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::create_coding_tools;

    #[test]
    fn default_prompt_includes_tools_guidelines_date_and_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let tools = create_coding_tools(dir.path(), None);
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: dir.path().to_path_buf(),
            tools,
            current_date: Some(Date::new(2026, 6, 17)),
            ..Default::default()
        });

        assert!(prompt.contains("You are an expert coding assistant operating inside Tau"));
        assert!(prompt.contains("Available tools:\n- read: Read file contents"));
        assert!(prompt.contains("- Use bash for file operations like ls, rg, find"));
        assert!(prompt.contains("- Use read to examine files instead of cat or sed."));
        assert!(prompt.ends_with(&format!(
            "Current date: 2026-06-17\nCurrent working directory: {}",
            dir.path().display()
        )));
    }

    #[test]
    fn tool_without_prompt_snippet_is_hidden() {
        let tool = AgentTool::new(
            "hidden",
            "Hidden",
            "Still sent to provider",
            serde_json::Map::new(),
            std::sync::Arc::new(|_, _, _, _| {
                Box::pin(async { Ok(rho_agent::tools::AgentToolResult::default()) })
            }),
        );
        assert_eq!(format_available_tools(&[tool]), "(none)");
    }

    #[test]
    fn guidelines_are_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let tools = create_coding_tools(dir.path(), None);
        let duplicate = tools[0].prompt_guidelines[0].clone();
        let guidelines = collect_prompt_guidelines(&tools, std::slice::from_ref(&duplicate));
        assert_eq!(guidelines.iter().filter(|g| **g == duplicate).count(), 1);
    }

    #[test]
    fn custom_prompt_replaces_default_but_keeps_append_context_and_date() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: dir.path().to_path_buf(),
            tools: create_coding_tools(dir.path(), None),
            custom_prompt: Some("Custom base.".into()),
            append_system_prompt: Some("Extra rules.".into()),
            context_files: vec![ProjectContextFile {
                path: "/repo/AGENTS.md".into(),
                content: "Follow rules.".into(),
            }],
            current_date: Some(Date::new(2026, 6, 17)),
            ..Default::default()
        });
        assert!(prompt.starts_with("Custom base.\n\nExtra rules."));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("<project_instructions path=\"/repo/AGENTS.md\">"));
        assert!(prompt.contains("Follow rules."));
        assert!(prompt.contains("Current date: 2026-06-17"));
    }

    #[test]
    fn empty_custom_prompt_is_still_custom() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: dir.path().to_path_buf(),
            tools: create_coding_tools(dir.path(), None),
            custom_prompt: Some(String::new()),
            append_system_prompt: Some("Extra rules.".into()),
            current_date: Some(Date::new(2026, 6, 17)),
            ..Default::default()
        });
        assert!(prompt.starts_with("\n\nExtra rules."));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("Current date: 2026-06-17"));
    }

    #[test]
    fn skills_are_formatted_as_xml_and_escaped() {
        let skill = Skill {
            name: "review&check".into(),
            path: PathBuf::from("/skills/review/SKILL.md"),
            description: "Review <code>".into(),
        };
        let formatted = format_skills_for_prompt(&[skill]);
        assert!(formatted.contains("<available_skills>"));
        assert!(formatted.contains("<name>review&amp;check</name>"));
        assert!(formatted.contains("<description>Review &lt;code&gt;</description>"));
        assert!(formatted.contains("<location>/skills/review/SKILL.md</location>"));
    }

    #[test]
    fn skills_included_only_when_read_tool_available() {
        let dir = tempfile::tempdir().unwrap();
        let skill = Skill {
            name: "testing".into(),
            path: dir.path().join("testing.md"),
            description: "Test".into(),
        };
        let no_read = AgentTool::new(
            "custom",
            "Custom",
            "Custom",
            serde_json::Map::new(),
            std::sync::Arc::new(|_, _, _, _| {
                Box::pin(async { Ok(rho_agent::tools::AgentToolResult::default()) })
            }),
        );
        let mut no_read = no_read;
        no_read.prompt_snippet = Some("Custom tool".into());

        let without_read = build_system_prompt(&BuildSystemPromptOptions {
            cwd: dir.path().to_path_buf(),
            tools: vec![no_read],
            skills: vec![skill.clone()],
            ..Default::default()
        });
        let with_read = build_system_prompt(&BuildSystemPromptOptions {
            cwd: dir.path().to_path_buf(),
            tools: create_coding_tools(dir.path(), None),
            skills: vec![skill],
            ..Default::default()
        });
        assert!(!without_read.contains("<available_skills>"));
        assert!(with_read.contains("<available_skills>"));
    }
}
