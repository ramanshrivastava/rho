//! Shared retry policy for provider adapters (tau `tau_ai/retry.py`).
//!
//! The delay curve, the transient-status set, and the cancellation-aware backoff
//! wait are ported exactly. rho does **not** carry tau's `ProviderRetryEvent`:
//! `canonicalize_provider_stream` dropped retry events at the Pi boundary
//! (`stream.py:71-73`), so a retry produces *no* canonical output. The rho
//! accumulator never sees a retry, so the only observable retry behavior is
//! "another HTTP attempt happens" — which the delay/wait helpers here govern.

use std::sync::Arc;
use std::time::Duration;

use rho_agent::provider::CancellationToken;

/// Poll granularity while waiting out a backoff (tau `RETRY_POLL_SECONDS`).
pub const RETRY_POLL_SECONDS: f64 = 0.05;
/// Base exponential delay (tau `RETRY_BASE_DELAY_SECONDS`).
pub const RETRY_BASE_DELAY_SECONDS: f64 = 0.25;

/// Return an exponential retry delay capped by provider config
/// (tau `retry_delay_seconds`).
#[must_use]
pub fn retry_delay_seconds(attempt: u32, max_delay_seconds: f64) -> f64 {
    if max_delay_seconds <= 0.0 {
        return 0.0;
    }
    let base_delay = RETRY_BASE_DELAY_SECONDS.min(max_delay_seconds);
    // `2 ** attempt` as f64; attempt is small (bounded by max_retries).
    let factor = 2f64.powi(i32::try_from(attempt).unwrap_or(i32::MAX));
    max_delay_seconds.min(base_delay * factor)
}

/// Whether a status code is transient/retryable (tau providers'
/// `_should_retry` status test and `_is_transient_status`).
#[must_use]
pub fn is_transient_status(status_code: u16) -> bool {
    matches!(status_code, 408 | 409 | 425 | 429) || status_code >= 500
}

/// Sleep before a retry while letting cancellation interrupt the backoff
/// (tau `wait_for_retry`).
///
/// Returns `true` if the caller may proceed with the retry, `false` if it should
/// abort (cancellation observed). A zero/negative delay still checks the signal
/// once, matching tau.
pub async fn wait_for_retry(
    delay_seconds: f64,
    signal: Option<&Arc<dyn CancellationToken>>,
) -> bool {
    let cancelled = || signal.is_some_and(|s| s.is_cancelled());
    if delay_seconds <= 0.0 {
        return !cancelled();
    }
    let mut remaining = delay_seconds;
    while remaining > 0.0 {
        if cancelled() {
            return false;
        }
        let step = RETRY_POLL_SECONDS.min(remaining);
        tokio::time::sleep(Duration::from_secs_f64(step)).await;
        remaining -= step;
    }
    !cancelled()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rho_agent::provider::{CancellationToken, SimpleCancellationToken};

    use super::*;

    #[test]
    fn delay_curve_matches_tau() {
        // max_delay 1.0: 0.25, 0.5, 1.0 (capped), 1.0 ...
        assert!((retry_delay_seconds(0, 1.0) - 0.25).abs() < 1e-12);
        assert!((retry_delay_seconds(1, 1.0) - 0.5).abs() < 1e-12);
        assert!((retry_delay_seconds(2, 1.0) - 1.0).abs() < 1e-12);
        assert!((retry_delay_seconds(3, 1.0) - 1.0).abs() < 1e-12);
        // max_delay 0 disables backoff entirely.
        assert!(retry_delay_seconds(5, 0.0).abs() < f64::EPSILON);
        // base is clamped to max_delay when max_delay < base.
        assert!((retry_delay_seconds(0, 0.1) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn transient_status_classes() {
        for code in [408, 409, 425, 429, 500, 502, 503, 529] {
            assert!(is_transient_status(code), "{code} should be transient");
        }
        for code in [400, 401, 403, 404, 422] {
            assert!(!is_transient_status(code), "{code} should not be transient");
        }
    }

    #[tokio::test]
    async fn wait_returns_true_without_signal() {
        assert!(wait_for_retry(0.0, None).await);
    }

    #[tokio::test]
    async fn cancellation_aborts_backoff_immediately() {
        let token = SimpleCancellationToken::new();
        token.cancel();
        let signal: Arc<dyn CancellationToken> = Arc::new(token);
        // A one-second delay must return instantly (false) once cancelled.
        assert!(!wait_for_retry(1.0, Some(&signal)).await);
        // Zero delay with a cancelled signal also aborts.
        assert!(!wait_for_retry(0.0, Some(&signal)).await);
    }
}
