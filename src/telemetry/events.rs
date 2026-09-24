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
        /// Opaque client-supplied correlation ID (e.g. an inbound
        /// `x-request-id` header), preserved for tracing/idempotency.
        /// This is NOT the database primary key — `request_id` is always
        /// gateway-generated so that a client reusing a correlation ID
        /// across distinct requests cannot overwrite another request's
        /// stored data. See docs/REMEDIATION_PLAN.md Phase 0.4.
        client_correlation_id: Option<String>,
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
        /// True when this timing was fabricated for a non-streaming
        /// response reconstructed into events after the fact (all such
        /// events land within microseconds of each other, since the whole
        /// response arrived in one HTTP round-trip) rather than observed
        /// from a genuine live stream. Analytical TTFB/TTFT/inter-chunk
        /// queries should exclude `synthetic = true` rows — averaging them
        /// in would silently understate real streaming latency
        /// (docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06).
        synthetic: bool,
    },

    /// First generation chunk received (TTFT).
    FirstGenerationChunk {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        content_preview: Option<String>,
        /// See `FirstProviderByte::synthetic`.
        synthetic: bool,
    },

    /// A stream chunk event (for timeline reconstruction).
    StreamChunk {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        sequence: u64,
        chunk_type: String,
        byte_count: u32,
        /// See `FirstProviderByte::synthetic`.
        synthetic: bool,
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
    ///
    /// docs/frontend/API_GAPS.md item 2 asked what "finality" means for a
    /// usage observation, since nothing previously distinguished a delta
    /// from a running total from a confirmed final count. Researched
    /// answer: every provider this gateway supports (OpenAI, OpenRouter,
    /// llama.cpp/llama-server, vLLM, Ollama, Gemini's OpenAI-compat layer)
    /// follows the same documented convention — usage is reported once per
    /// attempt as a cumulative total for the whole response (in the final
    /// SSE chunk when `stream_options.include_usage` is set, or in the
    /// single non-streaming response body), never as incrementally
    /// increasing per-chunk deltas. `kind` records this explicitly rather
    /// than leaving it assumed, so a future provider that actually streams
    /// deltas doesn't get silently misinterpreted.
    UsageRecorded {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        usage: Usage,
        /// Which code path produced this observation: `"stream_final"`
        /// (the last chunk of a genuine live SSE stream) or
        /// `"non_streaming"` (a single non-streaming response body,
        /// including the synthesized-event path used to give non-streaming
        /// responses a uniform internal representation — see
        /// `InstrumentedStream`'s `synthetic` field).
        source: String,
        /// Always `"cumulative"` today — see the variant doc comment.
        /// Reserved for `"delta"`/`"final"` if a provider is ever added
        /// that doesn't follow the single-cumulative-total convention.
        kind: String,
    },

    /// The completed generation content for one choice of one attempt.
    ///
    /// One event per choice index — an `n > 1` request emits one of these
    /// per choice, not one merged event for the whole attempt. Previously
    /// all choices' content was accumulated into a single `String` with no
    /// index at all, so a multi-choice response's individual choices were
    /// unrecoverable once persisted (docs/frontend/API_GAPS.md item 5's
    /// choice-ID gap).
    GenerationContent {
        request_id: Uuid,
        attempt_number: u32,
        timestamp: DateTime<Utc>,
        /// Which choice this content belongs to. `0` for the overwhelmingly
        /// common single-choice case.
        choice_index: u32,
        /// Full generated text content (critical for dataset goal).
        content: String,
        /// Full reasoning content (if exposed). Only ever populated for
        /// `choice_index == 0` — reasoning has no per-choice index in the
        /// provider-event model (`ProviderEvent::ReasoningDelta` carries no
        /// `index` field), and reasoning-capable models are not commonly
        /// combined with `n > 1` in practice. See the matching note in
        /// `src/providers/openai_compatible.rs`.
        reasoning_content: Option<String>,
        /// Tool calls for this choice (if any).
        tool_calls: Option<serde_json::Value>,
        /// Raw provider response body. Only ever populated on
        /// `choice_index == 0`'s event — there is one raw body per
        /// non-streaming attempt, not one per choice, so it would be
        /// wasteful and misleading to repeat it on every choice's row.
        raw_response: Option<serde_json::Value>,
        /// `"complete"` when reached via a genuine terminal `Completed`
        /// event; `"partial"` when reached via a mid-stream `Error` with
        /// whatever content had already accumulated preserved rather than
        /// discarded (Audit A04).
        status: String,
    },

    /// Cost information recorded.
    CostRecorded {
        request_id: Uuid,
        timestamp: DateTime<Utc>,
        estimated_cost: Option<f64>,
        actual_cost: Option<f64>,
        currency: String,
    },

    /// Point-in-time snapshot of a model alias's configuration — the
    /// `model_snapshots` table existed with no writer at all (Audit A15);
    /// this gives future attempts/generations a provenance record to
    /// reference even after the live config changes.
    ModelSnapshot {
        timestamp: DateTime<Utc>,
        model_name: String,
        providers: Vec<String>,
        capabilities: serde_json::Value,
    },

    /// Point-in-time snapshot of a provider's configuration and counters —
    /// same rationale as `ModelSnapshot`, for the `provider_snapshots`
    /// table.
    ProviderSnapshot {
        timestamp: DateTime<Utc>,
        provider_name: String,
        provider_type: String,
        health: String,
        total_requests: u64,
        total_errors: u64,
        avg_latency_ms: Option<f64>,
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
            Self::ModelSnapshot { .. } => "model_snapshot",
            Self::ProviderSnapshot { .. } => "provider_snapshot",
        }
    }

    /// Get the request ID this event belongs to. `ModelSnapshot` and
    /// `ProviderSnapshot` aren't tied to any single request — they return
    /// `Uuid::nil()` as an explicit "not applicable" sentinel rather than a
    /// real request ID.
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
            Self::ModelSnapshot { .. } | Self::ProviderSnapshot { .. } => Uuid::nil(),
        }
    }
}
