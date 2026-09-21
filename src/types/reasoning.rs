use serde::{Deserialize, Serialize};

// ─── Reasoning Data Model ────────────────────────────────────────────────────
//
// Explicitly models the different levels of reasoning visibility across
// providers. Prevents "no reasoning text returned" from being confused
// with "the model didn't reason."

/// How much reasoning content is visible from the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningVisibility {
    /// Full reasoning content is exposed.
    Exposed,
    /// Only a summary of the reasoning is provided.
    SummaryOnly,
    /// Only the token count is available.
    CountOnly,
    /// Reasoning occurred but no content is available.
    Unavailable,
    /// No reasoning was requested or performed.
    None,
}

/// Captured reasoning output from a generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningOutput {
    /// The level of reasoning visibility from the provider.
    pub visibility: ReasoningVisibility,

    /// Full exposed reasoning content (if `visibility == Exposed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Summary of reasoning (if `visibility == SummaryOnly`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Number of reasoning tokens used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,

    /// Encrypted reasoning content (some providers return this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,

    /// Provider-specific reasoning metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}
