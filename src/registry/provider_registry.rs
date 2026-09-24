use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::providers::Provider;
use crate::types::{ProviderConfig, ProviderHealth, ProviderStatus};

/// Per-provider circuit breaker state. See `ProviderRegistry::is_circuit_open`
/// and `record_request_end` for the transition logic
/// (docs/REMEDIATION_PLAN.md Phase 5 circuit breaker work).
#[derive(Debug, Clone, Copy)]
enum CircuitState {
    /// Normal operation — requests flow through.
    Closed,
    /// Tripped after `circuit_breaker_threshold` consecutive failures;
    /// routing skips this provider until `opened_at.elapsed()` exceeds the
    /// configured cooldown.
    Open { opened_at: Instant },
    /// Cooldown elapsed — one (or more, since this gateway doesn't
    /// serialize concurrent requests through a single trial slot) request
    /// is allowed through to test recovery. A failure here re-opens the
    /// circuit with a fresh cooldown; a success closes it.
    HalfOpen,
}

// ─── Provider Registry ───────────────────────────────────────────────────────
//
// Runtime registry of provider instances. Tracks:
// - Provider adapter instances (boxed trait objects)
// - Health state (updated by health check background task)
// - Request counters (for metrics)

pub struct ProviderRegistry {
    /// Provider name → adapter instance.
    providers: HashMap<String, Arc<dyn Provider>>,
    /// Provider name → runtime state (health, counters).
    state: DashMap<String, ProviderRuntimeState>,
}

struct ProviderRuntimeState {
    config: ProviderConfig,
    health: ProviderHealth,
    active_requests: u32,
    total_requests: u64,
    total_errors: u64,
    /// Inference-attempt latency only — see `record_latency`'s doc comment.
    total_latency_ms: u64,
    latency_samples: u64,
    /// Background health-probe latency only — see
    /// `record_health_check_latency`'s doc comment.
    total_health_check_latency_ms: u64,
    health_check_latency_samples: u64,
    circuit_state: CircuitState,
    consecutive_failures: u32,
    /// Fixed-window rate limiter state: the window's start time and how
    /// many attempts have been admitted since. See `is_rate_limited`.
    rate_limit_window: (Instant, u32),
    /// Ring buffer of the last `RECENT_OUTCOMES_CAPACITY` attempt outcomes
    /// (`true` = success), oldest first. Backs `effective_health`'s
    /// error-rate-based `Degraded` override — see that method's doc
    /// comment. Distinct from `total_requests`/`total_errors`, which are
    /// cumulative since startup and would make one bad hour on a
    /// long-running gateway invisible a week later.
    recent_outcomes: std::collections::VecDeque<bool>,
}

/// How many recent attempt outcomes `effective_health_of` considers.
const RECENT_OUTCOMES_CAPACITY: usize = 20;
/// Minimum recent outcomes required before the error-rate override can fire
/// — avoids a single failed request on a fresh or low-traffic provider
/// reading as "degraded" from one data point.
const RECENT_OUTCOMES_MIN_SAMPLES: usize = 5;

/// The health value actually reported to `/health`, `/metrics`,
/// `/providers`, and the periodic `ProviderSnapshot` telemetry event: the
/// raw ping-based `health`, overridden to `Degraded` when
/// `degraded_error_rate_threshold` is configured, at least
/// `RECENT_OUTCOMES_MIN_SAMPLES` recent outcomes have been recorded, and
/// their error rate meets or exceeds the threshold. Never overrides a
/// ping-determined `Unhealthy` — see `ProviderConfig::degraded_error_rate_threshold`'s
/// doc comment for why this only ever adds distrust, never removes it.
/// A free function (not a method taking `&self`) so it can be called both
/// from `ProviderRegistry::effective_health` (a single `.get()` lookup) and
/// from `all_statuses` (already holding a `dashmap` iterator entry) without
/// the latter re-entering the map and risking a same-shard lock conflict.
fn effective_health_of(state: &ProviderRuntimeState) -> ProviderHealth {
    if state.health == ProviderHealth::Unhealthy {
        return state.health;
    }
    let Some(threshold) = state.config.degraded_error_rate_threshold else {
        return state.health;
    };
    if state.recent_outcomes.len() < RECENT_OUTCOMES_MIN_SAMPLES {
        return state.health;
    }
    let failures = state.recent_outcomes.iter().filter(|ok| !**ok).count();
    let error_rate = failures as f64 / state.recent_outcomes.len() as f64;
    if error_rate >= threshold {
        ProviderHealth::Degraded
    } else {
        state.health
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            state: DashMap::new(),
        }
    }

    /// Register a provider adapter.
    pub fn register(
        &mut self,
        config: ProviderConfig,
        adapter: Arc<dyn Provider>,
    ) {
        let name = config.name.clone();
        self.state.insert(
            name.clone(),
            ProviderRuntimeState {
                config,
                health: ProviderHealth::Unknown,
                active_requests: 0,
                total_requests: 0,
                total_errors: 0,
                total_latency_ms: 0,
                latency_samples: 0,
                total_health_check_latency_ms: 0,
                health_check_latency_samples: 0,
                circuit_state: CircuitState::Closed,
                consecutive_failures: 0,
                rate_limit_window: (Instant::now(), 0),
                recent_outcomes: std::collections::VecDeque::with_capacity(RECENT_OUTCOMES_CAPACITY),
            },
        );
        self.providers.insert(name, adapter);
    }

    /// Get a provider adapter by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    /// Get all provider names.
    #[allow(dead_code)]
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// Get all provider adapter instances with their names.
    pub fn all_adapters(&self) -> Vec<(String, Arc<dyn Provider>)> {
        self.providers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Check if a provider exists and is enabled.
    pub fn is_available(&self, name: &str) -> bool {
        self.state
            .get(name)
            .map(|s| s.config.enabled && s.health != ProviderHealth::Unhealthy)
            .unwrap_or(false)
    }

    /// Check whether a provider is currently at (or over) its configured
    /// `max_concurrent` in-flight request limit. A provider with no
    /// `max_concurrent` configured is never at capacity. Previously this
    /// config field was parsed and stored but never read anywhere —
    /// accepted, then silently ignored, the same class of defect the
    /// original audits flagged for other unenforced fields
    /// (docs/REMEDIATION_PLAN.md Phase 5.3).
    pub fn is_at_capacity(&self, name: &str) -> bool {
        self.state
            .get(name)
            .map(|s| match s.config.max_concurrent {
                Some(limit) => s.active_requests >= limit,
                None => false,
            })
            .unwrap_or(false)
    }

    /// Check whether a provider is at its configured `rate_limit_per_sec`
    /// ceiling, admitting the attempt (incrementing the window's counter)
    /// if not. Disabled entirely (`false`, never mutating) when
    /// `rate_limit_per_sec` isn't configured.
    ///
    /// This mutates on every call, unlike `is_at_capacity` — rate limiting
    /// is inherently about *admission*, not a passive concurrency
    /// snapshot, so the check and the consumption of one slot in the
    /// window are the same operation (matches `is_circuit_open`'s
    /// check-and-transition pattern for the same reason).
    ///
    /// Fixed 1-second window, not a sliding/smoothed one: `window_start`
    /// resets to `now` and the count to zero once a full second has
    /// elapsed since it was last reset. This means up to `2x` the
    /// configured limit can theoretically be admitted across a window
    /// boundary (e.g. `limit` requests at 0.99s and another `limit` at
    /// 1.01s) — acceptable for protecting a single local inference server
    /// from being flooded, which is this feature's actual purpose, but
    /// worth knowing if this is ever repurposed as a strict SLA-grade
    /// limiter.
    pub fn is_rate_limited(&self, name: &str) -> bool {
        let Some(mut state) = self.state.get_mut(name) else {
            return false;
        };
        let Some(limit) = state.config.rate_limit_per_sec else {
            return false;
        };
        let (window_start, count) = state.rate_limit_window;
        if window_start.elapsed() >= Duration::from_secs(1) {
            state.rate_limit_window = (Instant::now(), 1);
            return false;
        }
        if count >= limit {
            return true;
        }
        state.rate_limit_window.1 += 1;
        false
    }

    /// Check whether a provider's circuit breaker is currently open (i.e.
    /// routing should skip it). Disabled entirely (`false`) when
    /// `circuit_breaker_threshold` isn't configured for this provider.
    ///
    /// This is the one circuit-breaker query that mutates state: an `Open`
    /// circuit whose cooldown has elapsed transitions to `HalfOpen` here
    /// (returning `false`, letting the trial request through) rather than
    /// on a separate timer, so the transition only ever happens in response
    /// to an actual routing check — there's nothing to drive it otherwise
    /// in a single-user gateway that may sit idle between requests.
    pub fn is_circuit_open(&self, name: &str) -> bool {
        let Some(mut state) = self.state.get_mut(name) else {
            return false;
        };
        let Some(_threshold) = state.config.circuit_breaker_threshold else {
            return false;
        };
        match state.circuit_state {
            CircuitState::Closed | CircuitState::HalfOpen => false,
            CircuitState::Open { opened_at } => {
                let cooldown = Duration::from_secs(state.config.circuit_breaker_cooldown_secs);
                if opened_at.elapsed() >= cooldown {
                    state.circuit_state = CircuitState::HalfOpen;
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Whether a provider's configured `base_url` is local (loopback or
    /// private-network) — backs the `local_first` routing policy. `false`
    /// for an unregistered provider (never preferred by a policy that
    /// couldn't route to it anyway).
    pub fn is_local(&self, name: &str) -> bool {
        self.state.get(name).map(|s| s.config.is_local()).unwrap_or(false)
    }

    /// Check if a provider is registered and administratively enabled,
    /// ignoring health status.
    ///
    /// Used by explicit-addressing routing short-circuits: explicit
    /// provider/model selection intentionally bypasses capability/priority
    /// filtering (that's the point of explicit addressing — reproducibility),
    /// but it must not bypass an operator's `enabled: false`, which is an
    /// administrative kill switch, not a routing preference. Health is
    /// deliberately excluded here — a caller who explicitly names a
    /// provider may know it's fine even if the periodic health probe
    /// hasn't caught up yet. See docs/REMEDIATION_PLAN.md Phase 1.4 /
    /// Audit A11.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.state.get(name).map(|s| s.config.enabled).unwrap_or(false)
    }

    /// Get the current health of a provider.
    #[allow(dead_code)]
    pub fn health(&self, name: &str) -> ProviderHealth {
        self.state
            .get(name)
            .map(|s| s.health)
            .unwrap_or(ProviderHealth::Unknown)
    }

    /// Update the health status of a provider.
    pub fn update_health(&self, name: &str, health: ProviderHealth) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.health = health;
        }
    }

    /// The health value actually reported to `/health`, `/metrics`,
    /// `/providers`, and the periodic `ProviderSnapshot` telemetry event:
    /// the raw ping-based `health`, overridden to `Degraded` when
    /// `degraded_error_rate_threshold` is configured, at least
    /// `RECENT_OUTCOMES_MIN_SAMPLES` recent outcomes have been recorded, and
    /// their error rate meets or exceeds the threshold. Never overrides a
    /// ping-determined `Unhealthy` — see the field's doc comment for why
    /// this is a "never remove trust, only add distrust" signal, not a
    /// replacement for the ping check.
    /// Record a request starting (for concurrency tracking).
    pub fn record_request_start(&self, name: &str) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.active_requests += 1;
            state.total_requests += 1;
        }
    }

    /// Record a request completing. Also drives the circuit breaker: a
    /// success resets the consecutive-failure count and closes an
    /// open/half-open circuit (recovery confirmed); a failure increments the
    /// count and, once it reaches `circuit_breaker_threshold` (if
    /// configured), opens the circuit with a fresh cooldown — including
    /// re-opening immediately if the failing request was itself the
    /// half-open trial.
    pub fn record_request_end(&self, name: &str, is_error: bool) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.active_requests = state.active_requests.saturating_sub(1);

            if state.recent_outcomes.len() >= RECENT_OUTCOMES_CAPACITY {
                state.recent_outcomes.pop_front();
            }
            state.recent_outcomes.push_back(!is_error);

            if is_error {
                state.total_errors += 1;
                state.consecutive_failures += 1;
                if let Some(threshold) = state.config.circuit_breaker_threshold
                    && state.consecutive_failures >= threshold
                {
                    state.circuit_state = CircuitState::Open {
                        opened_at: Instant::now(),
                    };
                }
            } else {
                state.consecutive_failures = 0;
                if !matches!(state.circuit_state, CircuitState::Closed) {
                    state.circuit_state = CircuitState::Closed;
                }
            }
        }
    }

    /// Record *inference* attempt latency for a provider — feeds
    /// `lowest_latency` routing and `/metrics`/`/providers` avg_latency_ms.
    /// Do not call this from the background health-check worker; use
    /// `record_health_check_latency` for that (Audit A13).
    pub fn record_latency(&self, name: &str, latency_ms: u64) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.total_latency_ms += latency_ms;
            state.latency_samples += 1;
        }
    }

    /// Record background health-probe latency for a provider. Kept in a
    /// separate rolling average from `record_latency` so periodic
    /// `/models` health checks never influence routing decisions or the
    /// reported inference-latency metric (docs/REMEDIATION_PLAN.md
    /// Phase 3.3 / Audit A13).
    pub fn record_health_check_latency(&self, name: &str, latency_ms: u64) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.total_health_check_latency_ms += latency_ms;
            state.health_check_latency_samples += 1;
        }
    }

    /// Get status for all providers (for /providers endpoint).
    pub fn all_statuses(&self) -> Vec<ProviderStatus> {
        self.state
            .iter()
            .map(|entry| {
                let s = entry.value();
                let avg_latency = if s.latency_samples > 0 {
                    Some(s.total_latency_ms as f64 / s.latency_samples as f64)
                } else {
                    None
                };
                let avg_health_check_latency = if s.health_check_latency_samples > 0 {
                    Some(s.total_health_check_latency_ms as f64 / s.health_check_latency_samples as f64)
                } else {
                    None
                };

                ProviderStatus {
                    name: entry.key().clone(),
                    provider_type: s.config.r#type,
                    health: effective_health_of(s),
                    enabled: s.config.enabled,
                    active_requests: s.active_requests,
                    total_requests: s.total_requests,
                    total_errors: s.total_errors,
                    avg_latency_ms: avg_latency,
                    avg_health_check_latency_ms: avg_health_check_latency,
                    circuit_state: match s.circuit_state {
                        CircuitState::Closed => "closed",
                        CircuitState::Open { .. } => "open",
                        CircuitState::HalfOpen => "half_open",
                    }
                    .to_string(),
                }
            })
            .collect()
    }
}
