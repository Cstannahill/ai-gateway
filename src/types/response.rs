use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::reasoning::ReasoningOutput;
use super::request::{ToolCall, Role};

// ─── Gateway Response ────────────────────────────────────────────────────────
//
// The canonical response representation. Provider responses are normalized into
// this before being sent to clients.

/// A complete (non-streaming) gateway response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayResponse {
    /// The request ID this responds to.
    pub request_id: Uuid,

    /// The model that actually served the request (resolved, not the alias).
    pub model: String,

    /// The provider that served the request.
    pub provider: String,

    /// Response choices.
    pub choices: Vec<Choice>,

    /// Aggregated usage information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,

    /// Reasoning output (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningOutput>,

    /// Timestamp when the response was completed.
    pub completed_at: DateTime<Utc>,

    /// End-to-end latency in milliseconds.
    pub latency_ms: u64,

    /// The provider's raw response ID (if provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_response_id: Option<String>,
}

/// A single choice/completion in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: Option<FinishReason>,
}

/// The generated message within a choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Why the generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
}

// ─── Usage ───────────────────────────────────────────────────────────────────

/// Canonical token usage information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,

    /// Cached input tokens (if reported by provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,

    /// Reasoning/thinking tokens (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,

    /// Provider's raw usage data (preserved for analysis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_provider_usage: Option<serde_json::Value>,
}

// ─── Streaming ───────────────────────────────────────────────────────────────

/// A single event in a streamed gateway response.
///
/// This is the canonical stream event that feeds both:
/// - The client serializer (SSE output)
/// - The telemetry recorder (observation capture)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStreamEvent {
    /// The request this event belongs to.
    pub request_id: Uuid,

    /// Monotonic sequence number within this stream.
    pub sequence: u64,

    /// High-resolution timestamp for this event.
    pub timestamp: DateTime<Utc>,

    /// The event payload.
    pub event: StreamEventKind,
}

/// The specific kind of stream event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEventKind {
    /// Stream has started; provider selected.
    #[serde(rename = "start")]
    Start {
        model: String,
        provider: String,
    },

    /// A delta of generated content.
    #[serde(rename = "content_delta")]
    ContentDelta {
        index: u32,
        delta: String,
    },

    /// A delta of reasoning/thinking content.
    #[serde(rename = "reasoning_delta")]
    ReasoningDelta {
        delta: String,
    },

    /// A delta of a tool call.
    #[serde(rename = "tool_call_delta")]
    ToolCallDelta {
        index: u32,
        tool_call_index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<String>,
    },

    /// Usage update (may arrive mid-stream or at end).
    #[serde(rename = "usage")]
    Usage(Usage),

    /// The stream has completed.
    #[serde(rename = "done")]
    Done {
        finish_reason: Option<FinishReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },

    /// An error occurred during streaming.
    #[serde(rename = "error")]
    Error {
        code: String,
        message: String,
    },
}
