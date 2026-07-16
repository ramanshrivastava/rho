//! Deterministic clock and id injection.
//!
//! tau makes its golden fixtures reproducible by monkeypatching three
//! module-level sources of nondeterminism *at their definition sites* before
//! constructing any model (`tau_agent.messages.time`,
//! `tau_agent.session.entries.time`, `tau_agent.session.entries.uuid4` — see
//! `tools/extract-fixtures/_common.py::patch_determinism`). Rust has no
//! monkeypatch, so rho threads the same three sources through explicit traits:
//!
//! * [`Clock::now_ms`] — integer-millisecond **message** timestamps
//!   (`current_timestamp_ms`).
//! * [`Clock::now_secs`] — float-second **session-entry** timestamps
//!   (`current_timestamp`).
//! * [`IdGen::new_id`] — 32-hex session-entry ids (`new_entry_id`).
//!
//! Production wiring uses [`SystemClock`] / [`UuidIdGen`], which defer to the M1
//! free functions ([`crate::messages::current_timestamp_ms`],
//! [`crate::session::entries::current_timestamp`] /
//! [`crate::session::entries::new_entry_id`]). Tests pin values with
//! [`FixedClock`] / [`SequentialIdGen`] so an event/session golden can be
//! reproduced byte-for-byte against tau's frozen extraction values
//! (`1_700_000_000_123` ms, `1_700_000_000.0` s, ids `"{n:032x}"`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::messages::current_timestamp_ms;
use crate::session::entries::{current_timestamp, new_entry_id};

/// Wall-clock source for message (`ms`) and session-entry (`float secs`) stamps.
///
/// The two granularities are deliberately different types, matching tau: message
/// `timestamp` is int milliseconds, session-entry `timestamp`/`created_at` are
/// float seconds.
pub trait Clock: Send + Sync {
    /// Current time in integer **milliseconds** (message timestamps).
    fn now_ms(&self) -> i64;

    /// Current time in float **seconds** (session-entry timestamps).
    fn now_secs(&self) -> f64;
}

/// Fresh-id source for session entries (tau `new_entry_id`).
pub trait IdGen: Send + Sync {
    /// Return a new unique id.
    fn new_id(&self) -> String;
}

/// Production clock: real wall time via the M1 timestamp helpers.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        current_timestamp_ms()
    }

    fn now_secs(&self) -> f64 {
        current_timestamp()
    }
}

/// Production id generator: `uuid4().hex` via the M1 helper.
#[derive(Debug, Clone, Copy, Default)]
pub struct UuidIdGen;

impl IdGen for UuidIdGen {
    fn new_id(&self) -> String {
        new_entry_id()
    }
}

/// A clock pinned to fixed values (tests / fixture reproduction).
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    /// Millisecond value returned by every [`Clock::now_ms`] call.
    pub ms: i64,
    /// Second value returned by every [`Clock::now_secs`] call.
    pub secs: f64,
}

impl FixedClock {
    /// Build a fixed clock. `ms` stamps messages; `secs` stamps session entries.
    #[must_use]
    pub fn new(ms: i64, secs: f64) -> Self {
        Self { ms, secs }
    }

    /// The exact values tau's fixture extraction freezes
    /// (`_FIXED_MESSAGE_TIME = 1_700_000_000.123` → `1_700_000_000_123` ms;
    /// `_FIXED_ENTRY_TIME = 1_700_000_000.0` s).
    #[must_use]
    pub fn fixture() -> Self {
        Self::new(1_700_000_000_123, 1_700_000_000.0)
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.ms
    }

    fn now_secs(&self) -> f64 {
        self.secs
    }
}

/// A monotonic id generator producing `"{n:032x}"` (tau's fixture counter).
///
/// Mirrors `_CounterUUID` in `tools/extract-fixtures/_common.py`: a process-local
/// counter formatted as 32 lowercase hex digits.
#[derive(Debug, Default)]
pub struct SequentialIdGen {
    counter: AtomicU64,
}

impl SequentialIdGen {
    /// Start a fresh counter at `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdGen for SequentialIdGen {
    fn new_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{n:032x}")
    }
}

/// The default production clock, shareable as `Arc<dyn Clock>`.
#[must_use]
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// The default production id generator, shareable as `Arc<dyn IdGen>`.
#[must_use]
pub fn uuid_id_gen() -> Arc<dyn IdGen> {
    Arc::new(UuidIdGen)
}
