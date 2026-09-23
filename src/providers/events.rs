use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Provider Events ─────────────────────────────────────────────────────────
//
// The event stream emitted by provider adapters during execution.
// These are the raw observations that feed both the client response
// serializer and the telemetry recorder.
//
// Telemetry becomes inherent to the execution model:
//
//   ProviderEvent
//        ↓
//   CanonicalGenerationEvent
//        ├──────────────► Client serializer
//        └──────────────► Telemetry recorder

/// Events emitted by a provider during request execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderEvent {
    /// Provider request initiated.
    Started {
        timestamp: DateTime<Utc>,
        provider: String,
        model: String,
    },

    /// TCP/TLS connection to provider established.
    Connected {
        timestamp: DateTime<Utc>,
        /// Time from Started to Connected in microseconds.
        connection_latency_us: u64,
    },

    /// HTTP response headers received from provider.
    ResponseHeaders {
        timestamp: DateTime<Utc>,
        status_code: u16,
        headers: HashMap<String, String>,
    },

    /// First byte of the response body received — distinct from `Connected`
    /// (which, for this adapter, is timestamped when headers arrive) and
    /// useful on its own for local/slow-to-start providers where there can
    /// be a real gap between headers and the first body byte (e.g. prompt
    /// processing time before a local LLM starts streaming). Emitted at
    /// most once per attempt. See docs/REMEDIATION_PLAN.md Phase 3.1.
    FirstByte { timestamp: DateTime<Utc> },

    /// A delta of reasoning/thinking content.
    ReasoningDelta {
        timestamp: DateTime<Utc>,
        delta: String,
    },

    /// A delta of generated content.
    ContentDelta {
        timestamp: DateTime<Utc>,
        index: u32,
        delta: String,
    },

    /// A delta of a tool call being constructed.
    ToolCallDelta {
        timestamp: DateTime<Utc>,
        index: u32,
        tool_call_index: u32,
        /// Tool call ID (present on first delta).
        id: Option<String>,
        /// Function name (present on first delta).
        name: Option<String>,
        /// Arguments fragment.
        arguments: Option<String>,
    },

    /// Usage information update (may arrive mid-stream or at completion).
    UsageUpdate {
        timestamp: DateTime<Utc>,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
        cached_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
        /// Provider's raw usage payload (preserved for JSONB storage).
        raw_usage: Option<serde_json::Value>,
    },

    /// Generation completed successfully.
    Completed {
        timestamp: DateTime<Utc>,
        finish_reason: Option<String>,
        /// The provider's response ID (if any).
        provider_response_id: Option<String>,
        /// Upstream provider info (e.g. DeepInfra behind OpenRouter).
        upstream_provider: Option<String>,
        /// The full raw provider response body, when a single body exists
        /// (non-streaming requests, where it's decoded in full before this
        /// event is constructed). `None` for real SSE streams, which have
        /// no single response body to preserve — previously this was
        /// always discarded even when available, so `store_raw_bodies`
        /// silently had no effect on this path (docs/REMEDIATION_PLAN.md
        /// Phase 3.6 / Audit A05).
        raw_response: Option<serde_json::Value>,
    },

    /// An error occurred during execution.
    Error {
        timestamp: DateTime<Utc>,
        code: String,
        message: String,
        /// HTTP status code (if applicable).
        status_code: Option<u16>,
        /// Whether this error is retryable.
        retryable: bool,
    },

    // ─── Non-streaming results ───────────────────────────────────────────

    /// Complete embedding result (non-streaming).
    EmbeddingResult {
        timestamp: DateTime<Utc>,
        embeddings: Vec<Vec<f32>>,
        model: String,
        usage: Option<EmbeddingUsage>,
    },

    /// Model listing result (non-streaming).
    ModelList {
        timestamp: DateTime<Utc>,
        models: Vec<crate::types::ModelInfo>,
    },
}

/// Usage information specific to embedding requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

impl ProviderEvent {
    /// Get the timestamp of this event.
    #[allow(dead_code)]
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::Started { timestamp, .. }
            | Self::Connected { timestamp, .. }
            | Self::ResponseHeaders { timestamp, .. }
            | Self::FirstByte { timestamp, .. }
            | Self::ReasoningDelta { timestamp, .. }
            | Self::ContentDelta { timestamp, .. }
            | Self::ToolCallDelta { timestamp, .. }
            | Self::UsageUpdate { timestamp, .. }
            | Self::Completed { timestamp, .. }
            | Self::Error { timestamp, .. }
            | Self::EmbeddingResult { timestamp, .. }
            | Self::ModelList { timestamp, .. } => *timestamp,
        }
    }

    /// Whether this event represents a terminal state.
    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Error { .. })
    }
}
