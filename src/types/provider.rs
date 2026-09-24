use serde::{Deserialize, Serialize};
use url::Url;

// ─── Provider Types ──────────────────────────────────────────────────────────

/// The type of provider — determines which adapter to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// llama.cpp / llama-server (OpenAI-compatible).
    LlamaCpp,
    /// OpenRouter API.
    OpenRouter,
    /// Ollama (native API).
    Ollama,
    /// Ollama Cloud.
    OllamaCloud,
    /// vLLM (OpenAI-compatible).
    Vllm,
    /// Google Gemini (Google AI, non-Vertex).
    Gemini,
    /// Hugging Face Inference API.
    HuggingFace,
    /// xAI / Grok API.
    Xai,
    /// Venice.ai.
    Venice,
    /// Featherless.ai.
    Featherless,
    /// Generic OpenAI-compatible provider.
    OpenAiCompatible,
}

/// Configuration for a single provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique name for this provider instance (e.g. "llama", "openrouter").
    pub name: String,

    /// The type of provider.
    pub r#type: ProviderType,

    /// Base URL for the provider's API.
    pub base_url: Url,

    /// Environment variable name that holds the API key (if needed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Static API key (prefer `api_key_env` in production).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Default headers to send with every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_headers: Option<std::collections::HashMap<String, String>>,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Maximum concurrent requests to this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,

    /// Consecutive attempt failures (see `ProviderRegistry::record_request_end`)
    /// before the circuit opens and this provider is skipped by routing.
    /// `None` (the default) disables the breaker entirely — matches
    /// `max_concurrent`'s opt-in shape so existing configs are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker_threshold: Option<u32>,

    /// How long the circuit stays open before allowing one trial request
    /// through (half-open) to test recovery. Only meaningful when
    /// `circuit_breaker_threshold` is set.
    #[serde(default = "default_circuit_breaker_cooldown_secs")]
    pub circuit_breaker_cooldown_secs: u64,

    /// Whether this provider is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_circuit_breaker_cooldown_secs() -> u64 {
    30
}

impl ProviderConfig {
    /// Whether this provider's `base_url` points at a loopback or
    /// private-network address — used by the `local_first` routing policy.
    /// Deliberately host-based rather than keyed off `r#type`: a generic
    /// `open_ai_compatible` entry pointing at a self-hosted vLLM/llama.cpp
    /// server on the LAN is just as "local" as a `llama_cpp`/`vllm`-typed
    /// one, and the type alone can't tell them apart.
    pub fn is_local(&self) -> bool {
        let Some(host) = self.base_url.host_str() else {
            return false;
        };
        if host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return match ip {
                std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
                std::net::IpAddr::V6(v6) => v6.is_loopback(),
            };
        }
        false
    }
}

fn default_timeout() -> u64 {
    120
}

fn default_true() -> bool {
    true
}

/// Runtime health state for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    /// Provider is healthy and accepting requests.
    Healthy,
    /// Provider is degraded (elevated errors or latency).
    Degraded,
    /// Provider is unhealthy / unreachable.
    Unhealthy,
    /// Provider health is unknown (not yet checked).
    Unknown,
}

/// Runtime state for a provider instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub provider_type: ProviderType,
    pub health: ProviderHealth,
    pub enabled: bool,

    /// Number of active requests.
    pub active_requests: u32,

    /// Total requests served since startup.
    pub total_requests: u64,

    /// Total errors since startup.
    pub total_errors: u64,

    /// Average *inference* latency in ms — recorded per completed request
    /// attempt, not per background health probe. Previously this field was
    /// fed by both, so `lowest_latency` routing and `/metrics` were mostly
    /// reflecting `/models` health-check timing rather than actual
    /// inference latency (docs/REMEDIATION_PLAN.md Phase 3.3 / Audit A13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_latency_ms: Option<f64>,

    /// Average background health-probe latency in ms — informational only,
    /// never used for routing decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_health_check_latency_ms: Option<f64>,

    /// Circuit breaker state: `"closed"` (normal), `"open"` (routing skips
    /// this provider entirely), or `"half_open"` (cooldown elapsed, one
    /// trial request is being let through to test recovery). Always
    /// `"closed"` when `circuit_breaker_threshold` isn't configured.
    pub circuit_state: String,
}
