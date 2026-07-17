//! Thinking-mode primitives for rho coding sessions (port of tau's
//! `tau_coding/thinking.py`).
//!
//! tau models the thinking level as a `Literal[...]`; rho keeps it as a plain
//! `String` ([`ThinkingLevel`]) validated against [`THINKING_LEVELS`]. Errors are
//! data: the fallible functions return `Result<_, String>` carrying tau's exact
//! user-facing error strings.
#![allow(clippy::doc_markdown)]

use std::collections::HashSet;

/// A validated Tau thinking level (tau's `ThinkingLevel` literal).
pub type ThinkingLevel = String;

/// The ordered set of supported thinking levels (tau `THINKING_LEVELS`).
pub const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

/// The default thinking level (tau `DEFAULT_THINKING_LEVEL`).
pub const DEFAULT_THINKING_LEVEL: &str = "medium";

/// Human-readable descriptions per level (tau `THINKING_LEVEL_DESCRIPTIONS`).
pub const THINKING_LEVEL_DESCRIPTIONS: [(&str, &str); 6] = [
    ("off", "No reasoning"),
    ("minimal", "Very brief reasoning"),
    ("low", "Light reasoning"),
    ("medium", "Moderate reasoning"),
    ("high", "Deep reasoning"),
    ("xhigh", "Maximum reasoning"),
];

/// Return a valid Tau thinking level or an error string (tau
/// `normalize_thinking_level`).
///
/// `None` maps to [`DEFAULT_THINKING_LEVEL`]; a value is trimmed and lowercased
/// before validation. The error message echoes the **original** value.
pub fn normalize_thinking_level(value: Option<&str>) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_THINKING_LEVEL.to_string());
    };
    let normalized = value.trim().to_lowercase();
    if THINKING_LEVELS.contains(&normalized.as_str()) {
        return Ok(normalized);
    }
    let allowed = THINKING_LEVELS.join(", ");
    Err(format!(
        "Unknown thinking mode: {value}. Available modes: {allowed}"
    ))
}

/// Return a validated, duplicate-free thinking-level list (tau
/// `normalize_thinking_levels`).
pub fn normalize_thinking_levels(values: &[String]) -> Result<Vec<String>, String> {
    if values.is_empty() {
        let allowed = THINKING_LEVELS.join(", ");
        return Err(format!(
            "Thinking modes must be a non-empty list. Available modes: {allowed}"
        ));
    }
    let normalized = values
        .iter()
        .map(|value| normalize_thinking_level(Some(value)))
        .collect::<Result<Vec<String>, String>>()?;
    let unique: HashSet<&String> = normalized.iter().collect();
    if unique.len() != normalized.len() {
        return Err("Thinking modes must be unique".to_string());
    }
    Ok(normalized)
}

/// Map Tau's UI thinking level to an OpenAI-compatible reasoning effort (tau
/// `reasoning_effort_for_level`). `"off"` becomes `"none"`.
pub fn reasoning_effort_for_level(level: Option<&str>) -> Result<String, String> {
    let normalized = normalize_thinking_level(level)?;
    if normalized == "off" {
        return Ok("none".to_string());
    }
    Ok(normalized)
}

/// Map Tau's UI thinking level to an Anthropic extended-thinking budget (tau
/// `anthropic_thinking_budget_for_level`). `"off"` yields `None`.
pub fn anthropic_thinking_budget_for_level(level: Option<&str>) -> Result<Option<i64>, String> {
    let normalized = normalize_thinking_level(level)?;
    let budget = match normalized.as_str() {
        "off" => return Ok(None),
        "minimal" => 1024,
        "low" => 2048,
        "medium" => 4096,
        "high" => 8192,
        "xhigh" => 16384,
        // Unreachable: `normalize_thinking_level` already constrained the value.
        _ => unreachable!("normalize_thinking_level guarantees a known level"),
    };
    Ok(Some(budget))
}

/// Return the next thinking level in a stable cycle (tau `next_thinking_level`).
///
/// Pass [`THINKING_LEVELS`] as `available` for the default cycle. On a parse or
/// lookup failure the first available level is returned; an empty `available`
/// yields [`DEFAULT_THINKING_LEVEL`].
#[must_use]
pub fn next_thinking_level(current: Option<&str>, available: &[&str]) -> String {
    if available.is_empty() {
        return DEFAULT_THINKING_LEVEL.to_string();
    }
    let index = match normalize_thinking_level(current) {
        Ok(normalized) => match available.iter().position(|&level| level == normalized) {
            Some(index) => index,
            None => return available[0].to_string(),
        },
        Err(_) => return available[0].to_string(),
    };
    available[(index + 1) % available.len()].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_thinking_level_accepts_supported_modes() {
        assert_eq!(normalize_thinking_level(Some("HIGH")).unwrap(), "high");
        assert_eq!(
            normalize_thinking_level(None).unwrap(),
            DEFAULT_THINKING_LEVEL
        );
    }

    #[test]
    fn normalize_thinking_level_rejects_unknown_mode() {
        let err = normalize_thinking_level(Some("maximum")).unwrap_err();
        assert!(err.contains("Unknown thinking mode"));
    }

    #[test]
    fn next_thinking_level_cycles_supported_modes() {
        assert_eq!(
            next_thinking_level(Some("medium"), &THINKING_LEVELS),
            "high"
        );
        assert_eq!(next_thinking_level(Some("xhigh"), &THINKING_LEVELS), "off");
        assert_eq!(
            next_thinking_level(Some("missing"), &["low", "high"]),
            "low"
        );
        assert_eq!(
            THINKING_LEVELS,
            ["off", "minimal", "low", "medium", "high", "xhigh"]
        );
    }

    #[test]
    fn normalize_thinking_levels_rejects_empty_and_duplicates() {
        assert_eq!(
            normalize_thinking_levels(&["OFF".to_string(), "high".to_string()]).unwrap(),
            vec!["off".to_string(), "high".to_string()]
        );

        let empty: Vec<String> = Vec::new();
        let err = normalize_thinking_levels(&empty).unwrap_err();
        assert!(err.contains("non-empty"));

        let dup_err =
            normalize_thinking_levels(&["high".to_string(), "HIGH".to_string()]).unwrap_err();
        assert!(dup_err.contains("unique"));
    }

    #[test]
    fn reasoning_effort_maps_off_to_none() {
        assert_eq!(reasoning_effort_for_level(Some("off")).unwrap(), "none");
        assert_eq!(reasoning_effort_for_level(Some("xhigh")).unwrap(), "xhigh");
    }
}
