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

impl GatewayConfig {
    /// Load configuration from a YAML file.
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let config: GatewayConfig = serde_yaml::from_str(&content)?;
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
        Ok(Self::default_config())
    }

    fn default_config() -> Self {
        Self {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            models: HashMap::new(),
            routing: RoutingConfig::default(),
            telemetry: TelemetryConfig::default(),
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
