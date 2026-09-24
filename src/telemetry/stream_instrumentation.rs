use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use pin_project_lite::pin_project;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::providers::events::ProviderEvent;
use crate::telemetry::events::{AttemptStatus, TelemetryEvent};
use crate::telemetry::health::TelemetryHealth;
use crate::types::Usage;

// ─── Stream Instrumentation ──────────────────────────────────────────────────
//
// Wraps a provider event stream to capture per-chunk telemetry
// WITHOUT buffering the entire generation.
//
//   Provider chunk ────────┐
//                          ├───── client (ProviderEvent)
//                          │
//                          └────► telemetry channel (TelemetryEvent)
//
// Every chunk gets a high-resolution timestamp and sequence number.
// Content is accumulated in-memory for the GenerationContent event
// at stream end (full text needed for dataset reconstruction).

/// Per-choice tool-call accumulator: choice index → tool_call index →
/// (id, function name, accumulated JSON arguments).
type ToolCallsMap = BTreeMap<u32, BTreeMap<u32, (Option<String>, Option<String>, String)>>;

pin_project! {
    pub struct InstrumentedStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = ProviderEvent> + Send>>,
        request_id: Uuid,
        attempt_number: u32,
        telemetry_tx: mpsc::Sender<TelemetryEvent>,
        telemetry_health: Arc<TelemetryHealth>,
        sequence: u64,
        first_content_seen: bool,
        // A monotonic clock, not `DateTime<Utc>` subtraction, for the
        // attempt-duration metric: wall-clock timestamps are not safe to
        // subtract across an NTP adjustment mid-request
        // (docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06). Captured at
        // construction time (immediately after `provider.execute()`
        // returns the stream), which is close enough to the adapter's own
        // `Started` event for this purpose.
        started_instant: std::time::Instant,
        // Keyed by choice index — an `n > 1` request accumulates each
        // choice's content separately so it can be persisted as its own
        // `GenerationContent` event/row instead of one merged blob
        // (docs/frontend/API_GAPS.md item 5's choice-ID gap).
        accumulated_content: BTreeMap<u32, String>,
        // Reasoning has no per-choice index in the provider-event model
        // (`ReasoningDelta` carries none) — always attributed to choice 0,
        // same scoping decision as `openai_compatible.rs`'s event parsing.
        accumulated_reasoning: String,
        // Outer key: choice index. Inner key: tool_call_index.
        tool_calls_map: ToolCallsMap,
        store_stream_events: bool,
        // True when `inner` is not a genuine live provider stream but a
        // one-shot sequence of events reconstructed from a single
        // non-streaming HTTP response (all landing within microseconds of
        // each other). Propagated into every timing-sensitive telemetry
        // event this wrapper emits so analytical queries can exclude
        // fabricated timing (docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06).
        synthetic: bool,
    }
}

impl InstrumentedStream {
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = ProviderEvent> + Send>>,
        request_id: Uuid,
        attempt_number: u32,
        telemetry_tx: mpsc::Sender<TelemetryEvent>,
        telemetry_health: Arc<TelemetryHealth>,
        store_stream_events: bool,
        synthetic: bool,
    ) -> Self {
        Self {
            inner,
            request_id,
            attempt_number,
            telemetry_tx,
            telemetry_health,
            sequence: 0,
            first_content_seen: false,
            started_instant: std::time::Instant::now(),
            accumulated_content: BTreeMap::new(),
            accumulated_reasoning: String::new(),
            tool_calls_map: BTreeMap::new(),
            store_stream_events,
            synthetic,
        }
    }
}

/// Fire-and-forget telemetry emit — never blocks the stream, but warns (and
/// records the drop for `/v1/analytics/persistence`) if the channel is full.
fn emit(tx: &mpsc::Sender<TelemetryEvent>, health: &TelemetryHealth, event: TelemetryEvent) {
    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(event) {
        tracing::warn!("Telemetry queue full; stream event dropped");
        health.record_dropped_event();
    }
}

/// Emits one `GenerationContent` event per choice index present in
/// `accumulated_content` or `tool_calls_map` — an `n > 1` attempt gets one
/// event per choice instead of one event merging every choice together
/// (docs/frontend/API_GAPS.md item 5's choice-ID gap). Reasoning and the
/// raw response body are only ever attached to choice 0's event; see the
/// matching field docs on `TelemetryEvent::GenerationContent`.
#[allow(clippy::too_many_arguments)]
fn emit_generation_content_per_choice(
    tx: &mpsc::Sender<TelemetryEvent>,
    health: &TelemetryHealth,
    request_id: Uuid,
    attempt_number: u32,
    timestamp: chrono::DateTime<chrono::Utc>,
    accumulated_content: &BTreeMap<u32, String>,
    accumulated_reasoning: &str,
    tool_calls_map: &ToolCallsMap,
    raw_response: Option<serde_json::Value>,
    status: &str,
) {
    let mut choice_indices: BTreeSet<u32> = accumulated_content.keys().copied().collect();
    choice_indices.extend(tool_calls_map.keys().copied());
    if choice_indices.is_empty() {
        // No content and no tool calls for any choice at all (e.g. an
        // attempt that failed before any delta arrived) — still emit a
        // single empty choice-0 event so a request always has *some*
        // generation row, matching the prior single-choice behavior.
        choice_indices.insert(0);
    }

    for choice_index in choice_indices {
        let content = accumulated_content.get(&choice_index).cloned().unwrap_or_default();

        let tool_calls_json: Vec<serde_json::Value> = tool_calls_map
            .get(&choice_index)
            .map(|calls| {
                calls
                    .values()
                    .map(|(id, name, args)| {
                        serde_json::json!({
                            "id": id.clone().unwrap_or_else(|| format!("call_{}", Uuid::now_v7())),
                            "type": "function",
                            "function": {
                                "name": name.clone().unwrap_or_default(),
                                "arguments": args.clone(),
                            }
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        emit(tx, health, TelemetryEvent::GenerationContent {
            request_id,
            attempt_number,
            timestamp,
            choice_index,
            content,
            reasoning_content: if choice_index == 0 && !accumulated_reasoning.is_empty() {
                Some(accumulated_reasoning.to_string())
            } else {
                None
            },
            tool_calls: if tool_calls_json.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&tool_calls_json).unwrap_or_default())
            },
            raw_response: if choice_index == 0 { raw_response.clone() } else { None },
            status: status.to_string(),
        });
    }
}

impl Stream for InstrumentedStream {
    type Item = ProviderEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        match this.inner.poll_next(cx) {
            Poll::Ready(Some(event)) => {
                *this.sequence += 1;
                let seq = *this.sequence;

                match &event {
                    ProviderEvent::Started { .. } => {
                        // ProviderAttemptStarted is emitted by the route
                        // handler *before* calling `provider.execute()`
                        // (docs/REMEDIATION_PLAN.md Phase 1.1 / Audit A03),
                        // so that connection failures and HTTP errors that
                        // never reach a stream are still recorded. Emitting
                        // it again here on the adapter's own `Started` event
                        // would double-record every attempt that does reach
                        // a stream. The attempt-duration clock
                        // (`started_instant`) was already captured at
                        // `InstrumentedStream::new` — nothing to do here.
                    }

                    ProviderEvent::Connected { timestamp, connection_latency_us } => {
                        emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::ProviderConnected {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            connection_latency_us: *connection_latency_us,
                        });
                    }

                    // Previously had no producer at all — the writer
                    // received `FirstProviderByte` events only from tests,
                    // never from real execution (docs/REMEDIATION_PLAN.md
                    // Phase 3.1 / Audit A06).
                    ProviderEvent::FirstByte { timestamp } => {
                        emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::FirstProviderByte {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            synthetic: *this.synthetic,
                        });
                    }

                    ProviderEvent::ContentDelta { timestamp, delta, index } => {
                        if !*this.first_content_seen {
                            *this.first_content_seen = true;
                            emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::FirstGenerationChunk {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                content_preview: Some(delta.chars().take(100).collect()),
                                synthetic: *this.synthetic,
                            });
                        }

                        this.accumulated_content.entry(*index).or_default().push_str(delta);

                        if *this.store_stream_events {
                            emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::StreamChunk {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                sequence: seq,
                                chunk_type: "content".to_string(),
                                byte_count: delta.len() as u32,
                                synthetic: *this.synthetic,
                            });
                        }
                    }

                    ProviderEvent::ReasoningDelta { timestamp, delta } => {
                        this.accumulated_reasoning.push_str(delta);

                        if *this.store_stream_events {
                            emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::StreamChunk {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                sequence: seq,
                                chunk_type: "reasoning".to_string(),
                                byte_count: delta.len() as u32,
                                synthetic: *this.synthetic,
                            });
                        }
                    }

                    ProviderEvent::ToolCallDelta {
                        timestamp,
                        index,
                        tool_call_index,
                        id,
                        name,
                        arguments,
                    } => {
                        let entry = this
                            .tool_calls_map
                            .entry(*index)
                            .or_default()
                            .entry(*tool_call_index)
                            .or_insert_with(|| (None, None, String::new()));
                        if let Some(id_str) = id {
                            entry.0 = Some(id_str.clone());
                        }
                        if let Some(name_str) = name {
                            entry.1 = Some(name_str.clone());
                        }
                        if let Some(args_str) = arguments {
                            entry.2.push_str(args_str);
                        }

                        if *this.store_stream_events {
                            emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::StreamChunk {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                sequence: seq,
                                chunk_type: "tool_call".to_string(),
                                byte_count: arguments.as_ref().map(|a| a.len() as u32).unwrap_or(0),
                                synthetic: *this.synthetic,
                            });
                        }
                    }

                    ProviderEvent::UsageUpdate {
                        timestamp,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_tokens,
                        reasoning_tokens,
                        raw_usage,
                    } => {
                        emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::UsageRecorded {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            usage: Usage {
                                prompt_tokens: prompt_tokens.unwrap_or(0),
                                completion_tokens: completion_tokens.unwrap_or(0),
                                total_tokens: total_tokens.unwrap_or(0),
                                cached_tokens: *cached_tokens,
                                reasoning_tokens: *reasoning_tokens,
                                raw_provider_usage: raw_usage.clone(),
                            },
                            source: if *this.synthetic { "non_streaming" } else { "stream_final" }.to_string(),
                            kind: "cumulative".to_string(),
                        });
                    }

                    ProviderEvent::Completed {
                        timestamp,
                        finish_reason,
                        provider_response_id,
                        upstream_provider,
                        raw_response,
                    } => {
                        let duration_us = this.started_instant.elapsed().as_micros() as u64;

                        emit_generation_content_per_choice(
                            this.telemetry_tx,
                            this.telemetry_health,
                            *this.request_id,
                            *this.attempt_number,
                            *timestamp,
                            this.accumulated_content,
                            this.accumulated_reasoning,
                            this.tool_calls_map,
                            raw_response.clone(),
                            "complete",
                        );

                        emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::ProviderAttemptCompleted {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            status: AttemptStatus::Success,
                            finish_reason: finish_reason.clone(),
                            provider_response_id: provider_response_id.clone(),
                            upstream_provider: upstream_provider.clone(),
                            duration_us,
                            error_code: None,
                            error_message: None,
                        });
                    }

                    ProviderEvent::Error {
                        timestamp,
                        code,
                        message,
                        ..
                    } => {
                        let duration_us = this.started_instant.elapsed().as_micros() as u64;

                        // A stream that produced partial content before
                        // failing must not lose that content — previously
                        // GenerationContent was only emitted on a clean
                        // Completed event, so an error mid-stream discarded
                        // everything generated so far (Audit A04).
                        if !this.accumulated_content.is_empty() || !this.accumulated_reasoning.is_empty() {
                            emit_generation_content_per_choice(
                                this.telemetry_tx,
                                this.telemetry_health,
                                *this.request_id,
                                *this.attempt_number,
                                *timestamp,
                                this.accumulated_content,
                                this.accumulated_reasoning,
                                this.tool_calls_map,
                                None,
                                "partial",
                            );
                        }

                        emit(this.telemetry_tx, this.telemetry_health, TelemetryEvent::ProviderAttemptCompleted {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            status: if code == "rate_limit" {
                                AttemptStatus::RateLimited
                            } else if code == "timeout" {
                                AttemptStatus::Timeout
                            } else {
                                AttemptStatus::Failed
                            },
                            finish_reason: None,
                            provider_response_id: None,
                            upstream_provider: None,
                            duration_us,
                            error_code: Some(code.clone()),
                            error_message: Some(message.clone()),
                        });
                    }

                    _ => {}
                }

                Poll::Ready(Some(event))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
