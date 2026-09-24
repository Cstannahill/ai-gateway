use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ─── Model Types ─────────────────────────────────────────────────────────────

/// A model definition in the registry — represents a logical model that may
/// be served by one or more providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    /// The canonical model name (e.g. "coding-fast", "long-context").
    pub name: String,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Candidate deployments, ordered by priority.
    pub candidates: Vec<ModelCandidate>,

    /// Known capabilities of this logical model.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// A specific model deployment on a specific provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCandidate {
    /// Provider identifier (e.g. "llama", "openrouter", "ollama").
    pub provider: String,

    /// The model ID as known to this provider.
    pub model: String,

    /// Priority for ordered routing (lower = higher priority).
    #[serde(default = "default_priority")]
    pub priority: u32,

    /// Override capabilities for this specific deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,

    /// Maximum context length for this deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_length: Option<u64>,

    /// Operator-declared quality rank for the `highest_quality`/`balanced`
    /// routing policies — lower is better (same convention as `priority`).
    /// There is no benchmark or eval data anywhere in this gateway to
    /// derive quality from automatically, so unlike `cheapest` (real
    /// pricing data) or `local_first` (real `base_url` data), this is
    /// explicit operator input, not gateway-inferred — the only honest way
    /// to implement "highest quality" without fabricating a score. Left
    /// unset, a candidate sorts last under these policies rather than being
    /// assumed best or worst.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_tier: Option<u32>,
}

fn default_priority() -> u32 {
    100
}

/// Capabilities a model deployment supports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Supports tool/function calling.
    #[serde(default)]
    pub tools: bool,

    /// Supports vision / image inputs.
    #[serde(default)]
    pub vision: bool,

    /// Supports reasoning/thinking mode.
    #[serde(default)]
    pub reasoning: bool,

    /// Supports structured output / JSON schema.
    #[serde(default)]
    pub structured_output: bool,

    /// Supports streaming.
    #[serde(default = "default_true")]
    pub streaming: bool,

    /// Supports embeddings.
    #[serde(default)]
    pub embeddings: bool,

    /// Maximum context window size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,

    /// Additional capability flags.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub extra: HashSet<String>,
}

fn default_true() -> bool {
    true
}

/// Model info returned by the `/v1/models` endpoint (OpenAI-compatible format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,

    /// Gateway-specific extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,

    /// Available providers for this model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

impl ModelInfo {
    pub fn new(name: &str, providers: Vec<String>, capabilities: Option<ModelCapabilities>) -> Self {
        Self {
            id: name.to_string(),
            object: "model".to_string(),
            created: 0,
            owned_by: "ai-gateway".to_string(),
            capabilities,
            providers,
        }
    }
}
