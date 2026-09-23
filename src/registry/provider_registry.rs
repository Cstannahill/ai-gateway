use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;

use crate::providers::Provider;
use crate::types::{ProviderConfig, ProviderHealth, ProviderStatus};

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

    /// Record a request starting (for concurrency tracking).
    pub fn record_request_start(&self, name: &str) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.active_requests += 1;
            state.total_requests += 1;
        }
    }

    /// Record a request completing.
    pub fn record_request_end(&self, name: &str, is_error: bool) {
        if let Some(mut state) = self.state.get_mut(name) {
            state.active_requests = state.active_requests.saturating_sub(1);
            if is_error {
                state.total_errors += 1;
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
                    health: s.health,
                    enabled: s.config.enabled,
                    active_requests: s.active_requests,
                    total_requests: s.total_requests,
                    total_errors: s.total_errors,
                    avg_latency_ms: avg_latency,
                    avg_health_check_latency_ms: avg_health_check_latency,
                }
            })
            .collect()
    }
}
