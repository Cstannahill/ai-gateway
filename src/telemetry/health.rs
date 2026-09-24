// ─── Telemetry Persistence Health ────────────────────────────────────────────
//
// Live observability into the telemetry write pipeline itself — is it
// actually flushing, how far behind, how many events have been dropped or
// are sitting in the fallback log — distinct from `/health`'s provider-only
// view, which says nothing about whether data capture is actually working.
// docs/frontend/API_GAPS.md item 8 (persistence-health half only; auth is
// explicitly out of scope for this personal, loopback-only deployment per
// the user's direction).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};

/// Which backend the telemetry writer is actually running against. Set
/// once at construction and never changed — a mid-run mode switch isn't
/// something this pipeline supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode {
    /// Connected to Postgres and writing for real.
    Database,
    /// Telemetry is enabled but no database was reachable at startup —
    /// events are logged, not persisted, until/unless the process restarts
    /// with connectivity restored (this writer does not retry the initial
    /// connection).
    Unavailable,
    /// `telemetry.enabled: false` in config — events are silently
    /// discarded by design, not a failure.
    Disabled,
}

impl TelemetryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Unavailable => "unavailable",
            Self::Disabled => "disabled",
        }
    }
}

/// Shared, updated-in-place telemetry pipeline health. One instance per
/// process, held as an `Arc` by `AppState` (for reads) and by every
/// component that can observe a real outcome — `TelemetryWriter` (flush
/// results, fallback-log state) and every `try_send` call site (dropped
/// events).
pub struct TelemetryHealth {
    mode: TelemetryMode,
    dropped_events: AtomicU64,
    last_flush_at: Mutex<Option<DateTime<Utc>>>,
    last_flush_ok: AtomicBool,
    consecutive_flush_failures: AtomicU32,
    /// Whether the fallback JSONL log currently has events sitting in it,
    /// undelivered — set on a failed flush that falls back to the log,
    /// cleared once `replay_fallback_log` confirms the file is empty/gone.
    fallback_pending: AtomicBool,
}

impl TelemetryHealth {
    pub fn new(mode: TelemetryMode) -> Self {
        Self {
            mode,
            dropped_events: AtomicU64::new(0),
            last_flush_at: Mutex::new(None),
            // No flush has failed because no flush has happened yet —
            // starting `true` avoids a misleading "unhealthy" reading
            // before the writer has done anything at all.
            last_flush_ok: AtomicBool::new(true),
            consecutive_flush_failures: AtomicU32::new(0),
            fallback_pending: AtomicBool::new(false),
        }
    }

    /// A telemetry event was dropped because the channel to the writer was
    /// full — the event never reached the writer at all, let alone the
    /// database. Called from every `try_send` call site
    /// (`AppState::emit_telemetry`, `InstrumentedStream`'s internal
    /// `emit`).
    pub fn record_dropped_event(&self) {
        self.dropped_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_flush_success(&self, at: DateTime<Utc>) {
        *self.last_flush_at.lock().unwrap() = Some(at);
        self.last_flush_ok.store(true, Ordering::Relaxed);
        self.consecutive_flush_failures.store(0, Ordering::Relaxed);
    }

    /// A flush exhausted its retries and fell back to the JSONL log rather
    /// than losing the batch outright — see `TelemetryWriter::flush_batch`.
    pub fn record_flush_failure(&self) {
        self.last_flush_ok.store(false, Ordering::Relaxed);
        self.consecutive_flush_failures.fetch_add(1, Ordering::Relaxed);
        self.fallback_pending.store(true, Ordering::Relaxed);
    }

    pub fn set_fallback_pending(&self, pending: bool) {
        self.fallback_pending.store(pending, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> TelemetryHealthSnapshot {
        TelemetryHealthSnapshot {
            mode: self.mode.as_str(),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
            last_flush_at: *self.last_flush_at.lock().unwrap(),
            last_flush_ok: self.last_flush_ok.load(Ordering::Relaxed),
            consecutive_flush_failures: self.consecutive_flush_failures.load(Ordering::Relaxed),
            fallback_pending: self.fallback_pending.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time read of `TelemetryHealth`, for serializing into the
/// `/v1/analytics/persistence` response without holding any locks while
/// building JSON.
pub struct TelemetryHealthSnapshot {
    pub mode: &'static str,
    pub dropped_events: u64,
    pub last_flush_at: Option<DateTime<Utc>>,
    pub last_flush_ok: bool,
    pub consecutive_flush_failures: u32,
    pub fallback_pending: bool,
}
