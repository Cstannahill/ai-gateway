use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use pin_project_lite::pin_project;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::providers::events::ProviderEvent;
use crate::telemetry::events::{AttemptStatus, TelemetryEvent};
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

pin_project! {
    pub struct InstrumentedStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = ProviderEvent> + Send>>,
        request_id: Uuid,
        attempt_number: u32,
        telemetry_tx: mpsc::Sender<TelemetryEvent>,
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
        accumulated_content: String,
        accumulated_reasoning: String,
        tool_calls_map: BTreeMap<u32, (Option<String>, Option<String>, String)>,
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
        store_stream_events: bool,
        synthetic: bool,
    ) -> Self {
        Self {
            inner,
            request_id,
            attempt_number,
            telemetry_tx,
            sequence: 0,
            first_content_seen: false,
            started_instant: std::time::Instant::now(),
            accumulated_content: String::new(),
            accumulated_reasoning: String::new(),
            tool_calls_map: BTreeMap::new(),
            store_stream_events,
            synthetic,
        }
    }
}

/// Fire-and-forget telemetry emit — never blocks the stream, but warns if channel is full.
fn emit(tx: &mpsc::Sender<TelemetryEvent>, event: TelemetryEvent) {
    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(event) {
        tracing::warn!("Telemetry queue full; stream event dropped");
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
                        emit(this.telemetry_tx, TelemetryEvent::ProviderConnected {
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
                        emit(this.telemetry_tx, TelemetryEvent::FirstProviderByte {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            synthetic: *this.synthetic,
                        });
                    }

                    ProviderEvent::ContentDelta { timestamp, delta, .. } => {
                        if !*this.first_content_seen {
                            *this.first_content_seen = true;
                            emit(this.telemetry_tx, TelemetryEvent::FirstGenerationChunk {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                content_preview: Some(delta.chars().take(100).collect()),
                                synthetic: *this.synthetic,
                            });
                        }

                        this.accumulated_content.push_str(delta);

                        if *this.store_stream_events {
                            emit(this.telemetry_tx, TelemetryEvent::StreamChunk {
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
                            emit(this.telemetry_tx, TelemetryEvent::StreamChunk {
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
                        tool_call_index,
                        id,
                        name,
                        arguments,
                        ..
                    } => {
                        let entry = this
                            .tool_calls_map
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
                            emit(this.telemetry_tx, TelemetryEvent::StreamChunk {
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
                        emit(this.telemetry_tx, TelemetryEvent::UsageRecorded {
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

                        let tool_calls_json: Vec<serde_json::Value> = this
                            .tool_calls_map
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
                            .collect();

                        // Emit the full generation content.
                        emit(this.telemetry_tx, TelemetryEvent::GenerationContent {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            content: this.accumulated_content.clone(),
                            reasoning_content: if this.accumulated_reasoning.is_empty() {
                                None
                            } else {
                                Some(this.accumulated_reasoning.clone())
                            },
                            tool_calls: if tool_calls_json.is_empty() {
                                None
                            } else {
                                Some(serde_json::to_value(&tool_calls_json).unwrap_or_default())
                            },
                            // Previously always None regardless of the
                            // `store_raw_bodies` config flag — now forwards
                            // whatever the adapter captured (the full body
                            // for non-streaming requests, None for a real
                            // SSE stream). The writer still gates storage on
                            // `store_raw_bodies` itself.
                            raw_response: raw_response.clone(),
                        });

                        emit(this.telemetry_tx, TelemetryEvent::ProviderAttemptCompleted {
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
                            let tool_calls_json: Vec<serde_json::Value> = this
                                .tool_calls_map
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
                                .collect();

                            emit(this.telemetry_tx, TelemetryEvent::GenerationContent {
                                request_id: *this.request_id,
                                attempt_number: *this.attempt_number,
                                timestamp: *timestamp,
                                content: this.accumulated_content.clone(),
                                reasoning_content: if this.accumulated_reasoning.is_empty() {
                                    None
                                } else {
                                    Some(this.accumulated_reasoning.clone())
                                },
                                tool_calls: if tool_calls_json.is_empty() {
                                    None
                                } else {
                                    Some(serde_json::to_value(&tool_calls_json).unwrap_or_default())
                                },
                                raw_response: None,
                            });
                        }

                        emit(this.telemetry_tx, TelemetryEvent::ProviderAttemptCompleted {
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
