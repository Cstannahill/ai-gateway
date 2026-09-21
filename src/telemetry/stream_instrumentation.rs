use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use chrono::{DateTime, Utc};
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
        started_at: Option<DateTime<Utc>>,
        accumulated_content: String,
        accumulated_reasoning: String,
        tool_calls_map: BTreeMap<u32, (Option<String>, Option<String>, String)>,
        store_stream_events: bool,
    }
}

impl InstrumentedStream {
    pub fn new(
        inner: Pin<Box<dyn Stream<Item = ProviderEvent> + Send>>,
        request_id: Uuid,
        attempt_number: u32,
        telemetry_tx: mpsc::Sender<TelemetryEvent>,
        store_stream_events: bool,
    ) -> Self {
        Self {
            inner,
            request_id,
            attempt_number,
            telemetry_tx,
            sequence: 0,
            first_content_seen: false,
            started_at: None,
            accumulated_content: String::new(),
            accumulated_reasoning: String::new(),
            tool_calls_map: BTreeMap::new(),
            store_stream_events,
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
                    ProviderEvent::Started { timestamp, provider, model } => {
                        *this.started_at = Some(*timestamp);
                        emit(this.telemetry_tx, TelemetryEvent::ProviderAttemptStarted {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            provider: provider.clone(),
                            model: model.clone(),
                        });
                    }

                    ProviderEvent::Connected { timestamp, connection_latency_us } => {
                        emit(this.telemetry_tx, TelemetryEvent::ProviderConnected {
                            request_id: *this.request_id,
                            attempt_number: *this.attempt_number,
                            timestamp: *timestamp,
                            connection_latency_us: *connection_latency_us,
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
                    } => {
                        let duration_us = this
                            .started_at
                            .map(|s| (*timestamp - s).num_microseconds().unwrap_or(0) as u64)
                            .unwrap_or(0);

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
                            raw_response: None,
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
                        let duration_us = this
                            .started_at
                            .map(|s| (*timestamp - s).num_microseconds().unwrap_or(0) as u64)
                            .unwrap_or(0);

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
