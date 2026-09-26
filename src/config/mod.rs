use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::types::{ModelDefinition, ProviderConfig};

// ─── Gateway Configuration ──────────────────────────────────────────────────
//
// YAML-driven configuration for the entire gateway: providers, models,
// routing defaults, server settings, and telemetry.

/// Top-level gateway configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    /// Server configuration.
    #[serde(default)]
    pub server: ServerConfig,

    /// Provider definitions, keyed by provider name.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Model definitions, keyed by model alias.
    #[serde(default)]
    pub models: HashMap<String, ModelDefinition>,

    /// Default routing configuration.
    #[serde(default)]
    pub routing: RoutingConfig,

    /// Telemetry / database configuration.
    #[serde(default)]
    pub telemetry: TelemetryConfig,

    /// Per-deployment pricing, keyed by `"provider/model"` (matching a
    /// `ModelCandidate`'s `provider`/`model` fields). Optional — deployments
    /// with no entry here simply produce no `cost_records` row, rather than
    /// a fabricated zero cost. Previously `cost_records`/`CostRecorded`
    /// existed with no producer at all (docs/REMEDIATION_PLAN.md Phase 3.5 /
    /// Audit A15/A17).
    #[serde(default)]
    pub pricing: HashMap<String, PricingEntry>,

    /// Exact-match response caching for non-streaming chat completions.
    /// Disabled by default — an opt-in feature (Phase 5 caching work), not
    /// a change to existing behavior.
    #[serde(default)]
    pub caching: CachingConfig,

    /// Local llama-server process management (`/llama-server/*`). Disabled
    /// by default — it is the only feature that executes a local binary.
    #[serde(default)]
    pub llama_server: LlamaServerConfig,
}

/// Configuration for the llama-server manager (`src/llama_server/`). See
/// `docs/CONFIGURATION_GUIDE.md` and `docs/frontend/LLAMA_SERVER_UI.md`.
#[derive(Debug, Clone, Deserialize)]
pub struct LlamaServerConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Path to the real `llama-server` executable (not a `.cmd` shim — a
    /// shim leaves the actual server orphaned when stopped).
    #[serde(default)]
    pub binary: Option<String>,

    /// Passed as `--host`, and where status probes connect.
    #[serde(default = "default_llama_host")]
    pub host: String,

    /// Passed as `--port`. Keep equal to the `llama` provider's port.
    #[serde(default = "default_llama_port")]
    pub port: u16,

    /// JSON file holding saved models and default args.
    #[serde(default = "default_llama_state_file")]
    pub state_file: String,

    /// Directories the model picker may list `*.gguf` files from. Nothing
    /// outside these is ever listed or accepted by the scan endpoint.
    #[serde(default)]
    pub scan_dirs: Vec<String>,

    /// Browser origins allowed to call `/llama-server/*` directly. Empty by
    /// default: only origin-less callers (the frontend's server-side proxy,
    /// curl) are accepted.
    #[serde(default)]
    pub allowed_origins: Vec<String>,

    /// Acknowledge that `server.host` is not loopback while this
    /// process-launching feature is enabled.
    #[serde(default)]
    pub allow_non_loopback: bool,

    /// How long to wait for the server to report ready before marking the
    /// start as failed. Large models load slowly.
    #[serde(default = "default_llama_start_timeout_secs")]
    pub start_timeout_secs: u64,

    /// Ring-buffer size for captured stdout/stderr lines.
    #[serde(default = "default_llama_log_lines")]
    pub log_lines: usize,
}

impl Default for LlamaServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            binary: None,
            host: default_llama_host(),
            port: default_llama_port(),
            state_file: default_llama_state_file(),
            scan_dirs: vec![],
            allowed_origins: vec![],
            allow_non_loopback: false,
            start_timeout_secs: default_llama_start_timeout_secs(),
            log_lines: default_llama_log_lines(),
        }
    }
}

fn default_llama_host() -> String {
    "127.0.0.1".to_string()
}

fn default_llama_port() -> u16 {
    8080
}

fn default_llama_state_file() -> String {
    "config/llama_models.json".to_string()
}

fn default_llama_start_timeout_secs() -> u64 {
    300
}

fn default_llama_log_lines() -> usize {
    2000
}

/// True for `localhost` and loopback IP literals.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Cost per 1,000 tokens for one provider/model deployment, in USD.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PricingEntry {
    pub input_cost_per_1k: f64,
    pub output_cost_per_1k: f64,
}

/// Response cache configuration. See `src/cache/mod.rs` and
/// `docs/ROUTING_AND_POLICIES.md`'s caching section for exactly what this
/// caches and why it's scoped to non-streaming requests only.
#[derive(Debug, Clone, Deserialize)]
pub struct CachingConfig {
    #[serde(default)]
    pub enabled: bool,

    /// How long a cached response stays valid.
    #[serde(default = "default_cache_ttl_secs")]
    pub ttl_secs: u64,

    /// Maximum distinct cached entries before the oldest-inserted one is
    /// evicted to make room.
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,
}

impl Default for CachingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_cache_ttl_secs(),
            max_entries: default_cache_max_entries(),
        }
    }
}

fn default_cache_ttl_secs() -> u64 {
    300
}

fn default_cache_max_entries() -> usize {
    1000
}

/// Server bind / runtime configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Host to bind to.
    #[serde(default = "default_host")]
    pub host: String,

    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Request body size limit in bytes.
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,

    /// Graceful shutdown timeout in seconds.
    #[serde(default = "default_shutdown_timeout")]
    pub shutdown_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            body_limit_bytes: default_body_limit(),
            shutdown_timeout_secs: default_shutdown_timeout(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8999
}

fn default_body_limit() -> usize {
    // 50 MB — generous for multimodal requests.
    50 * 1024 * 1024
}

fn default_shutdown_timeout() -> u64 {
    30
}

/// Default routing configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConfig {
    /// Default routing policy.
    #[serde(default = "default_policy")]
    pub default_policy: String,

    /// Maximum retry attempts per provider.
    #[serde(default = "default_max_retries")]
    pub max_retries_per_provider: u32,

    /// Conditions that trigger fallback to next provider.
    #[serde(default = "default_fallback_triggers")]
    pub fallback_on: Vec<String>,

    /// Request timeout in seconds (per attempt).
    #[serde(default = "default_attempt_timeout")]
    pub attempt_timeout_secs: u64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            default_policy: default_policy(),
            max_retries_per_provider: default_max_retries(),
            fallback_on: default_fallback_triggers(),
            attempt_timeout_secs: default_attempt_timeout(),
        }
    }
}

fn default_policy() -> String {
    "ordered".to_string()
}

fn default_max_retries() -> u32 {
    2
}

fn default_fallback_triggers() -> Vec<String> {
    vec![
        "timeout".to_string(),
        "rate_limit".to_string(),
        "unavailable".to_string(),
        "server_error".to_string(),
    ]
}

fn default_attempt_timeout() -> u64 {
    120
}

/// Telemetry / data pipeline configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// Whether telemetry recording is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// PostgreSQL connection URL.
    #[serde(default)]
    pub database_url: Option<String>,

    /// Environment variable holding the database URL (preferred).
    #[serde(default = "default_db_env")]
    pub database_url_env: String,

    /// Maximum database connection pool size.
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,

    /// Batch size for telemetry writes.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Flush interval in milliseconds.
    #[serde(default = "default_flush_interval")]
    pub flush_interval_ms: u64,

    /// Whether to store raw request/response bodies.
    #[serde(default = "default_true")]
    pub store_raw_bodies: bool,

    /// Whether to store individual stream chunk events.
    #[serde(default = "default_true")]
    pub store_stream_events: bool,

    /// Path to an append-only JSONL file used as a last-resort durability
    /// backstop when a batch fails to persist after all retries are
    /// exhausted. Not a full durable outbox (see docs/REMEDIATION_PLAN.md
    /// Phase 5.1) — just prevents an outage from silently discarding data.
    #[serde(default = "default_fallback_log_path")]
    pub fallback_log_path: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            database_url: None,
            database_url_env: default_db_env(),
            pool_size: default_pool_size(),
            batch_size: default_batch_size(),
            flush_interval_ms: default_flush_interval(),
            store_raw_bodies: true,
            store_stream_events: true,
            fallback_log_path: default_fallback_log_path(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_db_env() -> String {
    "DATABASE_URL".to_string()
}

fn default_pool_size() -> u32 {
    10
}

fn default_batch_size() -> usize {
    100
}

fn default_flush_interval() -> u64 {
    1000
}

fn default_fallback_log_path() -> String {
    "telemetry_fallback.jsonl".to_string()
}

/// Fallback-trigger names recognized by `routing.fallback_on`.
const KNOWN_FALLBACK_TRIGGERS: &[&str] =
    &["timeout", "rate_limit", "unavailable", "server_error", "content_filter"];

/// Routing policy names with real dispatch logic in `Router::route`. Every
/// `RoutingPolicy` enum variant (`src/routing/policy.rs`) has a dispatch arm
/// as of docs/REMEDIATION_PLAN.md's Phase 5 routing-policy work, so this
/// list and the enum are now in lockstep — a name outside this list is
/// simply not a recognized policy at all (typo), and both `default_policy`
/// (checked here at startup) and a per-request `routing.policy` override
/// (checked in `Router::route` at request time) reject it with the same
/// clear error rather than silently substituting ordered routing
/// (docs/REMEDIATION_PLAN.md Phase 1.5 / Audit CL03).
const IMPLEMENTED_ROUTING_POLICIES: &[&str] = &[
    "ordered",
    "priority",
    "round_robin",
    "lowest_latency",
    "fastest",
    "local_first",
    "cheapest",
    "highest_quality",
    "balanced",
];

impl GatewayConfig {
    /// Load configuration from a YAML file.
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let config: GatewayConfig = serde_yaml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Load with fallback: try the given path, then `config/gateway.yaml`,
    /// then `gateway.yaml`, then fall back to defaults.
    pub fn load(path: Option<&str>) -> anyhow::Result<Self> {
        if let Some(p) = path {
            return Self::from_file(p);
        }

        for candidate in &["config/gateway.yaml", "gateway.yaml"] {
            if Path::new(candidate).exists() {
                return Self::from_file(candidate);
            }
        }

        tracing::warn!("No configuration file found, using defaults");
        let config = Self::default_config();
        config.validate()?;
        Ok(config)
    }

    /// Reject configuration values that would otherwise fail silently or
    /// panic deep inside the telemetry/routing subsystems at runtime.
    ///
    /// See docs/REMEDIATION_PLAN.md Phase 0.6 and Phase 1.5.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.telemetry.batch_size == 0 {
            anyhow::bail!(
                "telemetry.batch_size must be greater than 0 (tokio::sync::mpsc::channel panics on a zero-capacity buffer)"
            );
        }
        if self.telemetry.flush_interval_ms == 0 {
            anyhow::bail!(
                "telemetry.flush_interval_ms must be greater than 0 (tokio::time::interval panics on a zero duration)"
            );
        }
        if self.telemetry.pool_size == 0 {
            anyhow::bail!("telemetry.pool_size must be greater than 0");
        }

        if !IMPLEMENTED_ROUTING_POLICIES.contains(&self.routing.default_policy.as_str()) {
            anyhow::bail!(
                "routing.default_policy '{}' is not an implemented policy. Implemented policies: {}",
                self.routing.default_policy,
                IMPLEMENTED_ROUTING_POLICIES.join(", ")
            );
        }

        for trigger in &self.routing.fallback_on {
            if !KNOWN_FALLBACK_TRIGGERS.contains(&trigger.as_str()) {
                anyhow::bail!(
                    "routing.fallback_on entry '{}' is not a recognized trigger. Known triggers: {}",
                    trigger,
                    KNOWN_FALLBACK_TRIGGERS.join(", ")
                );
            }
        }

        for (alias, definition) in &self.models {
            for candidate in &definition.candidates {
                if !self.providers.contains_key(&candidate.provider) {
                    anyhow::bail!(
                        "model '{}' references undefined provider '{}'",
                        alias,
                        candidate.provider
                    );
                }
            }
        }

        for (deployment, price) in &self.pricing {
            if price.input_cost_per_1k < 0.0 || price.output_cost_per_1k < 0.0 {
                anyhow::bail!("pricing entry '{}' has a negative cost per 1k tokens", deployment);
            }
        }

        for (name, provider) in &self.providers {
            // `Some(0)` for either of these means "this provider can never
            // serve a request" — always at capacity / always rate-limited
            // from the very first attempt. Nobody sets this intentionally;
            // it's a misconfiguration (meant to leave it unset for "no
            // limit", or meant to write a real number) that would otherwise
            // silently make the provider permanently unusable with no
            // startup error, only a confusing stream of
            // `provider_at_capacity`/`provider_rate_limited` rejections at
            // request time (docs/REMEDIATION_PLAN.md Phase 5 hardening
            // pass).
            if provider.max_concurrent == Some(0) {
                anyhow::bail!(
                    "provider '{}' has max_concurrent: 0, which would make it permanently at capacity — omit the field for no limit, or set a positive value",
                    name
                );
            }
            if provider.rate_limit_per_sec == Some(0) {
                anyhow::bail!(
                    "provider '{}' has rate_limit_per_sec: 0, which would make it permanently rate-limited — omit the field for no limit, or set a positive value",
                    name
                );
            }
            if let Some(threshold) = provider.degraded_error_rate_threshold
                && !(0.0..=1.0).contains(&threshold)
            {
                anyhow::bail!(
                    "provider '{}' has degraded_error_rate_threshold: {} — must be between 0.0 and 1.0 (it's a fraction, e.g. 0.5 for 50%, not a percentage)",
                    name,
                    threshold
                );
            }
        }

        self.validate_llama_server()?;

        Ok(())
    }

    /// Checks for the llama-server manager. Only enforced when enabled, so a
    /// half-filled section never blocks a gateway that isn't using it.
    fn validate_llama_server(&self) -> anyhow::Result<()> {
        let ls = &self.llama_server;
        if !ls.enabled {
            return Ok(());
        }
        let Some(binary) = ls.binary.as_deref().filter(|b| !b.trim().is_empty()) else {
            anyhow::bail!("llama_server.binary is required when llama_server.enabled is true");
        };
        if !Path::new(binary).is_file() {
            anyhow::bail!("llama_server.binary '{binary}' does not exist or is not a file");
        }
        let lower = binary.to_ascii_lowercase();
        if lower.ends_with(".cmd") || lower.ends_with(".bat") {
            anyhow::bail!(
                "llama_server.binary '{binary}' is a batch shim; point it at the real llama-server executable so stopping the server actually terminates it"
            );
        }
        if ls.port == 0 {
            anyhow::bail!("llama_server.port must be greater than 0");
        }
        if ls.log_lines == 0 {
            anyhow::bail!("llama_server.log_lines must be greater than 0");
        }
        if ls.start_timeout_secs == 0 {
            anyhow::bail!("llama_server.start_timeout_secs must be greater than 0");
        }
        if !is_loopback_host(&ls.host) && !ls.allow_non_loopback {
            anyhow::bail!(
                "llama_server.host '{}' is not loopback; the managed server would be reachable from the network. Set llama_server.allow_non_loopback: true to acknowledge",
                ls.host
            );
        }
        // This feature can launch a local process, so refuse to expose it on a
        // network-facing bind without an explicit acknowledgement.
        if !is_loopback_host(&self.server.host) && !ls.allow_non_loopback {
            anyhow::bail!(
                "llama_server is enabled but server.host '{}' is not loopback, so /llama-server/* (which starts a local process) would be reachable from the network. Bind server.host to 127.0.0.1, or set llama_server.allow_non_loopback: true to acknowledge",
                self.server.host
            );
        }
        Ok(())
    }

    fn default_config() -> Self {
        Self {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            models: HashMap::new(),
            routing: RoutingConfig::default(),
            telemetry: TelemetryConfig::default(),
            pricing: HashMap::new(),
            caching: CachingConfig::default(),
            llama_server: LlamaServerConfig::default(),
        }
    }

    /// Resolve the database URL from config or environment.
    pub fn database_url(&self) -> Option<String> {
        self.telemetry
            .database_url
            .clone()
            .or_else(|| std::env::var(&self.telemetry.database_url_env).ok())
    }
}
