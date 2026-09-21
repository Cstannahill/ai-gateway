use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::routing::RoutingDecision;
use crate::types::Usage;

// ─── Telemetry Events ────────────────────────────────────────────────────────
//
// Rich event types covering the full request lifecycle:
//
//   14:03:21.034  gateway received request
//   14:03:21.036  request normalized
//   14:03:21.037  route selected
//   14:03:21.039  provider request started
//   14:03:21.112  provider connection established
//   14:03:21.487  first provider byte
//   14:03:21.503  first generation chunk
//   14:03:21.503  chunk 1
//   14:03:21.537  chunk 2
//   ...
//   14:03:24.892  final chunk
//   14:03:24.895  provider stream closed
//   14:03:24.898  usage received
//   14:03:24.902  client stream closed
//
// From these, we derive (never pre-aggregate):
//   routing_latency, connection_latency, TTFB, TTFT,
//   prefill_estimate, generation_duration, inter_chunk_latency,
//   stream_duration, output_throughput, end-to-end_throughput,
//   provider_overhead, gateway_overhead

/// A telemetry event to be persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TelemetryEvent {
    /// A new inference request was received by the gateway.
    RequestReceived {
        request_id: Uuid,
        timestamp: DateTime<Utc>,
        /// The requesting application (from metadata).
        application: Option<String>,
        /// The task label (from metadata).
        task: Option<String>,
        /// User-defined tags.
        tags: Vec<String>,
        /// The model as requested by the client.
        requested_model: String,
        /// Whether streaming was requested.
        streaming: bool,
        /// Full canonical messages (for dataset reconstruction).
        canonical_messages: Option<serde_json::Value>,
        /// Raw request body (provider-native).
        raw_request: Option<serde_json::Value>,
        /// Feature flags for this request.
        uses_tools: bool,
        uses_vision: bool,
        uses_structured_output: bool,
        uses_reasoning: bool,
    },

    /// The routing decision was made.
    RouteSelected {
        request_id: Uuid,
        timestamp: DateTime<Utc>,
        decision: RoutingDecision,
    },

    /// A provider attempt was started.
    ProviderAttemptStarted {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        provider: String,
        model: String,
    },

    /// Provider TCP/TLS connection established.
    ProviderConnected {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        connection_latency_us: u64,
    },

    /// First byte received from provider (TTFB).
    FirstProviderByte {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
    },

    /// First generation chunk received (TTFT).
    FirstGenerationChunk {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        content_preview: Option<String>,
    },

    /// A stream chunk event (for timeline reconstruction).
    StreamChunk {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        sequence: u64,
        chunk_type: String,
        byte_count: u32,
    },

    /// A provider attempt completed.
    ProviderAttemptCompleted {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        status: AttemptStatus,
        finish_reason: Option<String>,
        provider_response_id: Option<String>,
        /// Upstream provider (e.g. DeepInfra behind OpenRouter).
        upstream_provider: Option<String>,
        /// Duration of this attempt in microseconds.
        duration_us: u64,
        /// Error details (if failed).
        error_code: Option<String>,
        error_message: Option<String>,
    },

    /// Token usage reported for an attempt.
    UsageRecorded {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        usage: Usage,
    },

    /// The completed generation content.
    GenerationContent {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        /// Full generated text content (critical for dataset goal).
        content: String,
        /// Full reasoning content (if exposed).
        reasoning_content: Option<String>,
        /// Tool calls (if any).
        tool_calls: Option<serde_json::Value>,
        /// Raw provider response body.
        raw_response: Option<serde_json::Value>,
    },

    /// Cost information recorded.
    CostRecorded {
        request_id: Uuid,
        timestamp: DateTime<Utc>,
        estimated_cost: Option<f64>,
        actual_cost: Option<f64>,
        currency: String,
    },

    /// The overall request completed (client stream closed).
    RequestCompleted {
        request_id: Uuid,
        timestamp: DateTime<Utc>,
        /// Total end-to-end latency in microseconds.
        total_latency_us: u64,
        /// Number of provider attempts made.
        total_attempts: u32,
        /// The provider that successfully served the request (if any).
        serving_provider: Option<String>,
        /// The model that actually served the request.
        resolved_model: Option<String>,
        /// Whether the request succeeded.
        success: bool,
    },
}

/// Status of a provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Success,
    Failed,
    Timeout,
    RateLimited,
    Cancelled,
}

impl TelemetryEvent {
    /// Get the event type as a string (for logging).
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::RequestReceived { .. } => "request_received",
            Self::RouteSelected { .. } => "route_selected",
            Self::ProviderAttemptStarted { .. } => "provider_attempt_started",
            Self::ProviderConnected { .. } => "provider_connected",
            Self::FirstProviderByte { .. } => "first_provider_byte",
            Self::FirstGenerationChunk { .. } => "first_generation_chunk",
            Self::StreamChunk { .. } => "stream_chunk",
            Self::ProviderAttemptCompleted { .. } => "provider_attempt_completed",
            Self::UsageRecorded { .. } => "usage_recorded",
            Self::GenerationContent { .. } => "generation_content",
            Self::CostRecorded { .. } => "cost_recorded",
            Self::RequestCompleted { .. } => "request_completed",
        }
    }

    /// Get the request ID this event belongs to.
    pub fn request_id(&self) -> Uuid {
        match self {
            Self::RequestReceived { request_id, .. }
            | Self::RouteSelected { request_id, .. }
            | Self::ProviderAttemptStarted { request_id, .. }
            | Self::ProviderConnected { request_id, .. }
            | Self::FirstProviderByte { request_id, .. }
            | Self::FirstGenerationChunk { request_id, .. }
            | Self::StreamChunk { request_id, .. }
            | Self::ProviderAttemptCompleted { request_id, .. }
            | Self::UsageRecorded { request_id, .. }
            | Self::GenerationContent { request_id, .. }
            | Self::CostRecorded { request_id, .. }
            | Self::RequestCompleted { request_id, .. } => *request_id,
        }
    }
}
