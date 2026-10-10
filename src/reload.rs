//! Configuration hot-reload.
//!
//! `POST /admin/reload` and a file-change watcher re-read `gateway.yaml` (and
//! the `.env` file) and apply what can be applied to the running gateway
//! without a restart:
//!
//! * **reloaded:** `providers`, `models`, `routing`;
//! * **restart required** (reported, never applied): `server` (the socket is
//!   bound), `telemetry` and `pricing` (the writer pipeline holds its own
//!   copy), `caching` (the cache instance), `llama_server` (it chooses the
//!   binary the gateway executes - never changed behind your back).
//!
//! The order is what makes a bad edit harmless: parse and validate the whole
//! new file, build every new adapter, and only then - after nothing can fail
//! any more - swap the live state. Anything that goes wrong leaves the running
//! configuration exactly as it was.
//!
//! In-flight requests are unaffected: each holds the configuration snapshot it
//! started with, and a provider adapter removed or replaced by a reload stays
//! alive until the requests using it finish. Providers whose configuration did
//! not change keep their adapter, so their health, counters, circuit breaker,
//! cooldown and API-key rotation state carry over.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::GatewayConfig;
use crate::config::env::{self, EnvFile};
use crate::providers::Provider;
use crate::server::AppState;
use crate::types::ProviderConfig;

/// How often the watcher looks at the files.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// Which sections cannot be reloaded, with the reason shown to the operator.
const RESTART_SECTIONS: &[&str] = &["server", "telemetry", "pricing", "caching", "llama_server"];

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Diff {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

impl Diff {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

/// The outcome of one reload attempt. Names only - never values.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadReport {
    pub at: DateTime<Utc>,
    /// `"endpoint"` or `"watch"`.
    pub trigger: String,
    /// `false`: the attempt was rejected and the running configuration is unchanged.
    pub ok: bool,
    /// Something was applied (a rejected or no-op reload is `false`).
    pub applied: bool,
    pub providers: Diff,
    pub models: Diff,
    /// Environment variables whose effective value changed.
    pub env_changed: Vec<String>,
    /// File variables a real environment variable overrides to a different value.
    pub env_shadowed: Vec<String>,
    /// Sections that differ from the running configuration but need a restart.
    pub restart_required: Vec<String>,
    pub error: Option<String>,
}

impl ReloadReport {
    fn new(trigger: &str) -> Self {
        Self {
            at: Utc::now(),
            trigger: trigger.to_string(),
            ok: true,
            applied: false,
            providers: Diff::default(),
            models: Diff::default(),
            env_changed: vec![],
            env_shadowed: vec![],
            restart_required: vec![],
            error: None,
        }
    }

    fn failed(mut self, error: impl Into<String>) -> Self {
        self.ok = false;
        self.applied = false;
        self.error = Some(error.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReloadStatus {
    pub config_file: Option<String>,
    pub env_file: Option<String>,
    pub reloads_applied: u64,
    pub last: Option<ReloadReport>,
}

/// Modification time and size of the watched files.
pub type FileSignature = Vec<Option<(SystemTime, u64)>>;

pub struct Reloader {
    /// The files as they were when the gateway started (and so as loaded) - the
    /// watcher's baseline. Taken here, not when the watcher task first runs, so
    /// an edit made in between is not mistaken for the loaded version.
    initial_signature: FileSignature,
    config_path: Option<PathBuf>,
    env: Option<EnvFile>,
    /// One reload at a time (endpoint and watcher can race).
    gate: tokio::sync::Mutex<()>,
    /// Provider name → fingerprint of the config + resolved keys its running
    /// adapter was built from.
    fingerprints: Mutex<HashMap<String, u64>>,
    status: Mutex<ReloadStatus>,
}

/// Hash of everything that determines how a provider adapter behaves: its
/// configuration and the key text it resolves to. Hashed, so no secret is kept.
fn fingerprint(config: &ProviderConfig) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(config).unwrap_or_default().hash(&mut hasher);
    let keys = config
        .api_key
        .clone()
        .or_else(|| config.api_key_env.as_deref().and_then(env::lookup));
    keys.hash(&mut hasher);
    hasher.finish()
}

/// `providers:` entries with the map key filled in as the name, as startup does.
fn named_providers(config: &GatewayConfig) -> Vec<ProviderConfig> {
    let mut providers: Vec<ProviderConfig> = config
        .providers
        .iter()
        .map(|(name, pc)| {
            let mut pc = pc.clone();
            if pc.name.is_empty() {
                pc.name = name.clone();
            }
            pc
        })
        .collect();
    providers.sort_by(|a, b| a.name.cmp(&b.name));
    providers
}

fn model_diff(old: &GatewayConfig, new: &GatewayConfig) -> Diff {
    let mut diff = Diff::default();
    for (name, definition) in &new.models {
        match old.models.get(name) {
            None => diff.added.push(name.clone()),
            Some(previous) => {
                if serde_json::to_value(previous).ok() != serde_json::to_value(definition).ok() {
                    diff.updated.push(name.clone());
                }
            }
        }
    }
    diff.removed = old.models.keys().filter(|n| !new.models.contains_key(*n)).cloned().collect();
    diff.added.sort();
    diff.updated.sort();
    diff.removed.sort();
    diff
}

fn restart_required(old: &GatewayConfig, new: &GatewayConfig) -> Vec<String> {
    let mut sections = Vec::new();
    if old.server != new.server {
        sections.push("server");
    }
    if old.telemetry != new.telemetry {
        sections.push("telemetry");
    }
    if pricing_differs(old, new) {
        sections.push("pricing");
    }
    if old.caching != new.caching {
        sections.push("caching");
    }
    if old.llama_server != new.llama_server {
        sections.push("llama_server");
    }
    debug_assert!(sections.iter().all(|s| RESTART_SECTIONS.contains(s)));
    sections.into_iter().map(str::to_string).collect()
}

fn pricing_differs(old: &GatewayConfig, new: &GatewayConfig) -> bool {
    let key = |c: &GatewayConfig| {
        let mut entries: Vec<_> = c
            .pricing
            .iter()
            .map(|(k, v)| (k.clone(), v.input_cost_per_1k.to_bits(), v.output_cost_per_1k.to_bits()))
            .collect();
        entries.sort();
        entries
    };
    key(old) != key(new)
}

/// The new configuration with every restart-only section kept at its running
/// value, so `AppState::config()` never claims something that is not in effect.
fn effective_config(running: &GatewayConfig, new: &GatewayConfig) -> GatewayConfig {
    let mut effective = new.clone();
    effective.server = running.server.clone();
    effective.telemetry = running.telemetry.clone();
    effective.pricing = running.pricing.clone();
    effective.caching = running.caching.clone();
    effective.llama_server = running.llama_server.clone();
    effective
}

impl Reloader {
    /// `config_path` is the file the gateway started from (`None` = built-in
    /// defaults, which cannot be reloaded); `env` is the env file, if any.
    /// `current` is the configuration the gateway is running.
    pub fn new(config_path: Option<PathBuf>, env: Option<EnvFile>, current: &GatewayConfig) -> Self {
        let fingerprints = named_providers(current)
            .iter()
            .map(|pc| (pc.name.clone(), fingerprint(pc)))
            .collect();
        let status = ReloadStatus {
            config_file: config_path.as_ref().map(|p| p.display().to_string()),
            env_file: env.as_ref().map(|e| e.path().display().to_string()),
            reloads_applied: 0,
            last: None,
        };
        let mut reloader = Self {
            initial_signature: Vec::new(),
            config_path,
            env,
            gate: tokio::sync::Mutex::new(()),
            fingerprints: Mutex::new(fingerprints),
            status: Mutex::new(status),
        };
        reloader.initial_signature = reloader.signature();
        reloader
    }

    pub fn status(&self) -> ReloadStatus {
        self.status.lock().unwrap().clone()
    }

    /// Modification time and size of the watched files - the watcher's change signal.
    fn signature(&self) -> FileSignature {
        let stat = |p: &std::path::Path| {
            std::fs::metadata(p).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        };
        vec![
            self.config_path.as_deref().and_then(stat),
            self.env.as_ref().and_then(|e| stat(e.path())),
        ]
    }

    /// Re-read the configuration and apply it. Never panics and never leaves
    /// the gateway half-reloaded; the report says what happened.
    pub async fn reload(&self, state: &AppState, trigger: &str) -> ReloadReport {
        let _gate = self.gate.lock().await;
        let report = self.reload_locked(state, trigger);
        {
            let mut status = self.status.lock().unwrap();
            if report.applied {
                status.reloads_applied += 1;
            }
            status.last = Some(report.clone());
        }
        report
    }

    fn reload_locked(&self, state: &AppState, trigger: &str) -> ReloadReport {
        let mut report = ReloadReport::new(trigger);

        let Some(path) = self.config_path.as_ref() else {
            return report.failed(
                "the gateway is running on built-in defaults (no config file was found at startup), so there is nothing to reload",
            );
        };

        // 1. Parse + validate the new file. No side effects yet.
        let new_config = match GatewayConfig::from_file(path) {
            Ok(c) => c,
            Err(e) => return report.failed(format!("{e:#}")),
        };
        let running = state.config();
        let effective = effective_config(&running, &new_config);
        if let Err(e) = effective.validate() {
            return report.failed(format!("{e:#}"));
        }
        report.restart_required = restart_required(&running, &new_config);

        // 2. Re-read the env file into the overlay. From here on a failure
        //    must put the overlay back.
        let env_snapshot = env::snapshot();
        if let Some(env_file) = &self.env {
            match env_file.reload() {
                Ok(change) => {
                    report.env_changed = change.changed;
                    report.env_shadowed = change.shadowed;
                }
                Err(e) => return report.failed(e),
            }
        }

        // 3. Build adapters for providers that are new or changed.
        let wanted = named_providers(&effective);
        let old_fingerprints = self.fingerprints.lock().unwrap().clone();
        let mut new_fingerprints = HashMap::new();
        let mut desired: Vec<(ProviderConfig, Option<Arc<dyn Provider>>)> = Vec::new();
        for pc in wanted {
            let fp = fingerprint(&pc);
            new_fingerprints.insert(pc.name.clone(), fp);
            let unchanged = old_fingerprints.get(&pc.name) == Some(&fp)
                && state.provider_registry.get(&pc.name).is_some();
            if unchanged {
                desired.push((pc, None));
                continue;
            }
            match crate::providers::build_registered_adapter(&pc, state.llama_server.as_ref()) {
                Ok(adapter) => desired.push((pc, Some(adapter))),
                Err(e) => {
                    env::restore(env_snapshot);
                    return report.failed(format!("provider '{}': {e:#}", pc.name));
                }
            }
        }

        // 4. Nothing below can fail. Swap.
        report.models = model_diff(&running, &effective);
        let summary = state.provider_registry.reconcile(desired);
        report.providers = Diff {
            added: summary.added,
            updated: summary.updated,
            removed: summary.removed,
        };
        state.model_registry.replace(effective.models.clone());
        state.router.set_default_policy(effective.routing.default_policy.clone());
        let routing_changed = running.routing != effective.routing;
        state.config.set(effective.clone());
        *self.fingerprints.lock().unwrap() = new_fingerprints;

        report.applied = !report.providers.is_empty()
            || !report.models.is_empty()
            || !report.env_changed.is_empty()
            || routing_changed;

        // Provenance for telemetry: models whose definition changed or appeared.
        for name in report.models.added.iter().chain(&report.models.updated) {
            if let Some(definition) = effective.models.get(name) {
                state.emit_telemetry(crate::telemetry::TelemetryEvent::ModelSnapshot {
                    timestamp: Utc::now(),
                    model_name: name.clone(),
                    providers: definition.candidates.iter().map(|c| c.provider.clone()).collect(),
                    capabilities: serde_json::to_value(&definition.capabilities).unwrap_or_default(),
                });
            }
        }
        report
    }
}

/// Log a finished reload: names and counts only.
pub fn log_report(report: &ReloadReport) {
    if !report.ok {
        tracing::warn!(
            trigger = %report.trigger,
            error = report.error.as_deref().unwrap_or(""),
            "Config reload rejected; the running configuration is unchanged"
        );
        return;
    }
    if report.applied {
        tracing::info!(
            trigger = %report.trigger,
            providers_added = %report.providers.added.join(","),
            providers_updated = %report.providers.updated.join(","),
            providers_removed = %report.providers.removed.join(","),
            models_added = %report.models.added.join(","),
            models_updated = %report.models.updated.join(","),
            models_removed = %report.models.removed.join(","),
            env_changed = %report.env_changed.join(","),
            "Config reloaded"
        );
    } else {
        tracing::info!(trigger = %report.trigger, "Config reload: no changes");
    }
    if !report.restart_required.is_empty() {
        tracing::warn!(
            sections = %report.restart_required.join(", "),
            "These config sections changed but cannot be reloaded; restart the gateway to apply them"
        );
    }
    if !report.env_shadowed.is_empty() {
        tracing::warn!(
            variables = %report.env_shadowed.join(", "),
            "Set in the env file to a different value than in the real environment; the real environment wins"
        );
    }
}

/// Poll the config and env files and reload when either changes. A change is
/// applied only once it has been stable for a full poll, so an editor's
/// half-written save does not trigger a reload; a version that fails to load
/// is reported once and not retried until the file changes again.
pub fn spawn_watcher(state: Arc<AppState>, shutdown: tokio::sync::oneshot::Receiver<()>) -> tokio::task::JoinHandle<()> {
    spawn_watcher_with_interval(state, shutdown, WATCH_INTERVAL)
}

/// [`spawn_watcher`] with an explicit poll interval (tests use a short one).
pub fn spawn_watcher_with_interval(
    state: Arc<AppState>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(reloader) = state.reloader.clone() else { return };
        let mut applied_signature = reloader.initial_signature.clone();
        let mut pending: Option<FileSignature> = None;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let current = reloader.signature();
                    if current == applied_signature {
                        pending = None;
                        continue;
                    }
                    if pending.as_ref() == Some(&current) {
                        let report = reloader.reload(&state, "watch").await;
                        log_report(&report);
                        applied_signature = current;
                        pending = None;
                    } else {
                        pending = Some(current);
                    }
                }
                _ = &mut shutdown => {
                    tracing::info!("Config watcher shutting down");
                    return;
                }
            }
        }
    })
}
