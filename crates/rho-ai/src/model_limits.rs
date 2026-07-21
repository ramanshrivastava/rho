//! Optional runtime model-limit discovery contracts for provider adapters
//! (tau `tau_ai/model_limits.py`).
//!
//! Some serving surfaces (notably the ChatGPT-subscription Codex catalog) report
//! per-model context limits that vary by rollout/account, so the static catalog
//! is only a conservative fallback. A provider that can discover live limits
//! returns them as [`RuntimeModelLimits`]; the session prefers them over the
//! configured catalog when present.

/// Provider-reported limits for one model on the active serving surface (tau
/// `RuntimeModelLimits`).
///
/// Construct via [`RuntimeModelLimits::new`], which enforces tau's
/// `__post_init__` invariants (all counts positive; the effective-window
/// percent in `1..=100`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelLimits {
    /// The provider's reported context window for the model, in tokens.
    context_window: i64,
    /// The provider's reported max output tokens, if any.
    max_output_tokens: Option<i64>,
    /// Percentage of the window the provider considers usable (`1..=100`).
    effective_context_window_percent: i64,
    /// An explicit auto-compaction limit, if the provider reports one.
    auto_compact_token_limit: Option<i64>,
}

impl RuntimeModelLimits {
    /// Build limits, validating tau's `RuntimeModelLimits.__post_init__`
    /// invariants. Returns the tau error message on violation.
    pub fn new(
        context_window: i64,
        max_output_tokens: Option<i64>,
        effective_context_window_percent: i64,
        auto_compact_token_limit: Option<i64>,
    ) -> Result<Self, String> {
        if context_window <= 0 {
            return Err("context_window must be positive".to_string());
        }
        if let Some(max_output_tokens) = max_output_tokens {
            if max_output_tokens <= 0 {
                return Err("max_output_tokens must be positive".to_string());
            }
        }
        if !(1..=100).contains(&effective_context_window_percent) {
            return Err("effective_context_window_percent must be between 1 and 100".to_string());
        }
        if let Some(auto_compact_token_limit) = auto_compact_token_limit {
            if auto_compact_token_limit <= 0 {
                return Err("auto_compact_token_limit must be positive".to_string());
            }
        }
        Ok(Self {
            context_window,
            max_output_tokens,
            effective_context_window_percent,
            auto_compact_token_limit,
        })
    }

    /// The provider's reported context window, in tokens.
    #[must_use]
    pub fn context_window(&self) -> i64 {
        self.context_window
    }

    /// The provider's reported max output tokens, if any.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<i64> {
        self.max_output_tokens
    }

    /// The provider's usable window after its requested headroom (tau
    /// `effective_context_window`).
    #[must_use]
    pub fn effective_context_window(&self) -> i64 {
        (self.context_window * self.effective_context_window_percent / 100).max(1)
    }

    /// An explicit auto-compaction limit or the Codex-compatible 90% default,
    /// clamped to the effective window (tau `effective_auto_compact_token_limit`).
    #[must_use]
    pub fn effective_auto_compact_token_limit(&self) -> i64 {
        let default_limit = (self.context_window * 9 / 10).max(1);
        match self.auto_compact_token_limit {
            None => default_limit.min(self.effective_context_window()),
            Some(limit) => limit.min(self.effective_context_window()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_positive_context_window() {
        assert_eq!(
            RuntimeModelLimits::new(0, None, 100, None).unwrap_err(),
            "context_window must be positive"
        );
    }

    #[test]
    fn rejects_out_of_range_percent() {
        assert_eq!(
            RuntimeModelLimits::new(100, None, 0, None).unwrap_err(),
            "effective_context_window_percent must be between 1 and 100"
        );
        assert_eq!(
            RuntimeModelLimits::new(100, None, 101, None).unwrap_err(),
            "effective_context_window_percent must be between 1 and 100"
        );
    }

    #[test]
    fn rejects_non_positive_optional_counts() {
        assert_eq!(
            RuntimeModelLimits::new(100, Some(0), 100, None).unwrap_err(),
            "max_output_tokens must be positive"
        );
        assert_eq!(
            RuntimeModelLimits::new(100, None, 100, Some(0)).unwrap_err(),
            "auto_compact_token_limit must be positive"
        );
    }

    #[test]
    fn effective_window_applies_headroom_percent() {
        let limits = RuntimeModelLimits::new(200_000, None, 90, None).unwrap();
        assert_eq!(limits.effective_context_window(), 180_000);
    }

    #[test]
    fn effective_window_is_at_least_one() {
        // 1 token * 1% floors to 0; tau clamps to 1.
        let limits = RuntimeModelLimits::new(1, None, 1, None).unwrap();
        assert_eq!(limits.effective_context_window(), 1);
    }

    #[test]
    fn auto_compact_defaults_to_ninety_percent_clamped() {
        // No explicit limit: min(90% of window, effective window).
        let limits = RuntimeModelLimits::new(200_000, None, 100, None).unwrap();
        assert_eq!(limits.effective_auto_compact_token_limit(), 180_000);

        // Effective window smaller than the 90% default clamps down.
        let limits = RuntimeModelLimits::new(200_000, None, 50, None).unwrap();
        assert_eq!(limits.effective_auto_compact_token_limit(), 100_000);
    }

    #[test]
    fn auto_compact_uses_explicit_limit_clamped() {
        let limits = RuntimeModelLimits::new(200_000, None, 100, Some(50_000)).unwrap();
        assert_eq!(limits.effective_auto_compact_token_limit(), 50_000);

        // Explicit limit above the effective window clamps to the window.
        let limits = RuntimeModelLimits::new(200_000, None, 50, Some(150_000)).unwrap();
        assert_eq!(limits.effective_auto_compact_token_limit(), 100_000);
    }
}
