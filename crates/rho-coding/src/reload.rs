//! Reload summary types for rho coding-session resources.
//!
//! Port of tau's `tau_coding/reload.py`. `/reload` recomputes the counts of
//! skills, prompt templates, extensions, project-context files, and resource
//! diagnostics, then reports the before/after delta per category. The types are
//! deliberately data-only; the async reload lifecycle lives on `CodingSession`
//! (tau keeps the extension lifecycle hooks out of the synchronous command
//! registry — see `commands.py::_reload_command`).

/// Before/after state for one reload category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReloadCategorySummary {
    /// Count before the reload.
    pub before: usize,
    /// Count after the reload.
    pub after: usize,
    /// Whether the underlying resource set changed.
    pub changed: bool,
}

impl ReloadCategorySummary {
    /// Construct a category summary.
    #[must_use]
    pub fn new(before: usize, after: usize, changed: bool) -> Self {
        Self {
            before,
            after,
            changed,
        }
    }

    /// Return the count delta for this category (`after - before`).
    ///
    /// tau exposes this as a Python `int` (signed); the Rust port returns
    /// `isize` so the `/reload` formatter can render `+N`/`-N` deltas.
    #[must_use]
    pub fn delta(&self) -> isize {
        let after = isize::try_from(self.after).unwrap_or(isize::MAX);
        let before = isize::try_from(self.before).unwrap_or(isize::MAX);
        after.saturating_sub(before)
    }
}

/// Summary of a local coding-resource reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodingReloadSummary {
    /// Skills category summary.
    pub skills: ReloadCategorySummary,
    /// Prompt-templates category summary.
    pub prompt_templates: ReloadCategorySummary,
    /// Project-context-files category summary.
    pub context_files: ReloadCategorySummary,
    /// Extensions category summary.
    pub extensions: ReloadCategorySummary,
    /// Resource-diagnostics category summary.
    pub diagnostics: ReloadCategorySummary,
    /// Whether the next-turn system prompt was rebuilt.
    pub system_prompt_rebuilt: bool,
}
