//! RFC 8628-style device authorization polling helpers.
//!
//! Port of tau's `tau_coding/oauth_device.py`. The core [`poll_oauth_device_code`]
//! implements RFC 8628 timing (interval flooring, `slow_down` back-off, expiry
//! deadline) and cooperative cancellation. tau injects `sleep`/`monotonic` for
//! deterministic tests; rho keeps that seam via [`poll_device_code_with`], with
//! [`poll_oauth_device_code`] wiring the real tokio sleep + monotonic clock.

#![allow(clippy::doc_markdown)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;

use crate::oauth::OAuthError;

/// Cooperative cancellation signal (tau's `asyncio.Event`).
#[derive(Debug, Default)]
pub struct CancelSignal {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl CancelSignal {
    /// Build a fresh, un-set cancellation signal.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Request cancellation and wake any pending wait.
    pub fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    async fn notified(&self) {
        // Match `asyncio.Event.wait()` (tau `oauth_device.py`), which returns
        // immediately when the event is already set. tokio's `Notify` stores no
        // permit for `notify_waiters()`, so a `set()` landing between the flag
        // check and waiter registration would otherwise be lost. Register the
        // waiter first (`enable()`), *then* observe any prior `set()`.
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.flag.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// Status of one device-token polling request (tau `DevicePollStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePollStatus {
    /// Authorization succeeded; `value` carries the credential.
    Complete,
    /// Authorization still pending; keep polling.
    Pending,
    /// The server asked us to slow the polling cadence.
    SlowDown,
    /// Authorization failed permanently.
    Failed,
}

/// Result of one device-token polling request (tau `DevicePollResult`).
#[derive(Debug, Clone)]
pub struct DevicePollResult<T> {
    /// The poll status.
    pub status: DevicePollStatus,
    /// The credential value (only for [`DevicePollStatus::Complete`]).
    pub value: Option<T>,
    /// A failure message (only for [`DevicePollStatus::Failed`]).
    pub message: Option<String>,
    /// A server-supplied interval override (only for [`DevicePollStatus::SlowDown`]).
    pub interval_seconds: Option<f64>,
}

impl<T> DevicePollResult<T> {
    /// A `pending` result.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            status: DevicePollStatus::Pending,
            value: None,
            message: None,
            interval_seconds: None,
        }
    }

    /// A `complete` result carrying `value`.
    #[must_use]
    pub fn complete(value: T) -> Self {
        Self {
            status: DevicePollStatus::Complete,
            value: Some(value),
            message: None,
            interval_seconds: None,
        }
    }

    /// A `failed` result with `message`.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: DevicePollStatus::Failed,
            value: None,
            message: Some(message.into()),
            interval_seconds: None,
        }
    }

    /// A `slow_down` result with an optional interval override.
    #[must_use]
    pub fn slow_down(interval_seconds: Option<f64>) -> Self {
        Self {
            status: DevicePollStatus::SlowDown,
            value: None,
            message: None,
            interval_seconds,
        }
    }
}

/// Poll an OAuth device flow with RFC 8628 timing and cancellation, using the
/// real tokio sleep and monotonic clock (tau `poll_oauth_device_code`).
pub async fn poll_oauth_device_code<T, P, Fut>(
    poll: P,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
    wait_before_first_poll: bool,
    cancel: Option<Arc<CancelSignal>>,
) -> Result<T, OAuthError>
where
    P: FnMut() -> Fut,
    Fut: Future<Output = DevicePollResult<T>>,
{
    let base = Instant::now();
    poll_device_code_with(
        poll,
        interval_seconds,
        expires_in_seconds,
        wait_before_first_poll,
        cancel,
        // `Duration::from_secs_f64` panics on an over-large finite value; a
        // server-supplied `interval` / `slow_down` reaches here, so use the
        // fallible conversion and saturate (tau's `asyncio.sleep` never panics).
        |seconds| {
            Box::pin(tokio::time::sleep(
                Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX),
            ))
        },
        move || base.elapsed().as_secs_f64(),
    )
    .await
}

/// Poll an OAuth device flow with injectable `sleep`/`monotonic` seams (tau's
/// keyword-injected `sleep`/`monotonic`).
#[allow(clippy::too_many_arguments)]
pub async fn poll_device_code_with<T, P, Fut, S, M>(
    mut poll: P,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
    wait_before_first_poll: bool,
    cancel: Option<Arc<CancelSignal>>,
    mut sleep: S,
    mut monotonic: M,
) -> Result<T, OAuthError>
where
    P: FnMut() -> Fut,
    Fut: Future<Output = DevicePollResult<T>>,
    S: FnMut(f64) -> BoxFuture<'static, ()>,
    M: FnMut() -> f64,
{
    let mut interval = poll_interval(interval_seconds);
    let deadline = expires_in_seconds.map_or(f64::INFINITY, |secs| monotonic() + secs);
    if wait_before_first_poll {
        let wait_secs = interval.min((deadline - monotonic()).max(0.0));
        wait(wait_secs, cancel.as_ref(), &mut sleep).await?;
    }

    let mut saw_slow_down = false;
    while monotonic() < deadline {
        raise_if_cancelled(cancel.as_ref())?;
        let result = poll().await;
        match result.status {
            DevicePollStatus::Complete => {
                return result
                    .value
                    .ok_or_else(|| OAuthError("Device flow returned no credential".to_string()));
            }
            DevicePollStatus::Failed => {
                return Err(OAuthError(
                    result
                        .message
                        .unwrap_or_else(|| "Device authorization failed".to_string()),
                ));
            }
            DevicePollStatus::SlowDown => {
                saw_slow_down = true;
                interval = match result.interval_seconds {
                    Some(value) => poll_interval(Some(value)),
                    None => interval + 5.0,
                };
            }
            DevicePollStatus::Pending => {}
        }

        let remaining = deadline - monotonic();
        if remaining <= 0.0 {
            break;
        }
        wait(interval.min(remaining), cancel.as_ref(), &mut sleep).await?;
    }

    let suffix = if saw_slow_down {
        " after one or more slow_down responses"
    } else {
        ""
    };
    Err(OAuthError(format!("Device flow timed out{suffix}")))
}

fn poll_interval(value: Option<f64>) -> f64 {
    match value {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => seconds.max(1.0),
        _ => 5.0,
    }
}

async fn wait<S>(
    seconds: f64,
    cancel: Option<&Arc<CancelSignal>>,
    sleep: &mut S,
) -> Result<(), OAuthError>
where
    S: FnMut(f64) -> BoxFuture<'static, ()>,
{
    raise_if_cancelled(cancel)?;
    if seconds <= 0.0 {
        return Ok(());
    }
    match cancel {
        None => {
            sleep(seconds).await;
            Ok(())
        }
        Some(cancel) => {
            tokio::select! {
                () = sleep(seconds) => Ok(()),
                () = cancel.notified() => {
                    if cancel.is_set() {
                        Err(OAuthError("Login cancelled".to_string()))
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }
}

fn raise_if_cancelled(cancel: Option<&Arc<CancelSignal>>) -> Result<(), OAuthError> {
    if cancel.is_some_and(|cancel| cancel.is_set()) {
        Err(OAuthError("Login cancelled".to_string()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[tokio::test]
    async fn slow_down_backs_off_then_completes() {
        let sleeps: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
        let results = Rc::new(RefCell::new(
            vec![
                DevicePollResult::<String>::slow_down(None),
                DevicePollResult::complete("done".to_string()),
            ]
            .into_iter(),
        ));

        let results_for_poll = results.clone();
        let poll = move || {
            let next = results_for_poll.borrow_mut().next().unwrap();
            async move { next }
        };
        let sleeps_for_sleep = sleeps.clone();
        let sleep = move |seconds: f64| {
            sleeps_for_sleep.borrow_mut().push(seconds);
            Box::pin(async {}) as BoxFuture<'static, ()>
        };
        let base = Instant::now();
        let monotonic = move || base.elapsed().as_secs_f64();

        let value =
            poll_device_code_with(poll, Some(1.0), Some(60.0), false, None, sleep, monotonic)
                .await
                .unwrap();

        assert_eq!(value, "done");
        assert_eq!(*sleeps.borrow(), vec![6.0]);
    }

    #[tokio::test]
    async fn cancelled_before_first_poll_raises() {
        let cancel = CancelSignal::new();
        cancel.set();
        let poll = || async { DevicePollResult::<String>::pending() };
        let error = poll_oauth_device_code(poll, None, None, false, Some(cancel))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test]
    async fn notified_returns_when_set_before_wait() {
        // Regression: a `set()` that lands before `notified()` registers a
        // waiter must still wake it — matching `asyncio.Event.wait()`'s
        // retained-set semantics. tokio's `notify_waiters()` stores no permit,
        // so without the `enable()` + flag-check this `notified()` would hang.
        let cancel = CancelSignal::new();
        cancel.set();
        tokio::time::timeout(Duration::from_secs(5), cancel.notified())
            .await
            .expect("notified() must return immediately when already set");
    }
}
