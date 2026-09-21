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

    /// Whether this provider is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
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

    /// Average latency in ms (rolling window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_latency_ms: Option<f64>,
}
