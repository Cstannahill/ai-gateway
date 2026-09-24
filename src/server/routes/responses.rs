use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use futures::StreamExt;
use uuid::Uuid;

use crate::providers::events::ProviderEvent;
use crate::providers::ProviderOutput;
use crate::routing::fallback;
use crate::server::AppState;
use crate::telemetry::events::{AttemptStatus, TelemetryEvent};
use crate::telemetry::stream_instrumentation::InstrumentedStream;
use crate::types::request::{GatewayRequest, Message, MessageContent, Role};

/// POST /v1/responses
///
/// OpenAI Responses API endpoint.
/// Adapts the Responses API request format to the canonical gateway model,
/// executes via an available provider, and formats the output accordingly.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    state.record_gateway_request();

    // `request_id` is always gateway-generated — see the note in
    // chat_completions.rs::handle and docs/REMEDIATION_PLAN.md Phase 0.4.
    let request_id = Uuid::now_v7();
    let client_correlation_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let received_at = Utc::now();

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "missing_model",
                "Request must include a valid 'model' parameter",
            );
        }
    };

    let mut messages = Vec::new();

    if let Some(instructions) = body.get("instructions").and_then(|i| i.as_str()) {
        messages.push(Message {
            role: Role::System,
            content: Some(MessageContent::Text(instructions.to_string())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        });
    }

    if let Some(input_val) = body.get("input") {
        if let Some(s) = input_val.as_str() {
            messages.push(Message {
                role: Role::User,
                content: Some(MessageContent::Text(s.to_string())),
                name: None,
                tool_call_id: None,
                tool_calls: None,
            });
        } else if let Some(arr) = input_val.as_array() {
            for item in arr {
                if let Some(text) = item.as_str() {
                    messages.push(Message {
                        role: Role::User,
                        content: Some(MessageContent::Text(text.to_string())),
                        name: None,
                        tool_call_id: None,
                        tool_calls: None,
                    });
                } else if let Some(message) = parse_input_message_object(item) {
                    // Standard Responses API object-form input items
                    // (`{"role": "...", "content": "..."}`) were previously
                    // silently dropped — only plain strings survived
                    // (Audit A09).
                    messages.push(message);
                }
            }
        }
    }

    let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let request = GatewayRequest {
        request_id,
        received_at,
        model: model.clone(),
        messages: Some(messages),
        input: body.get("input").cloned(),
        temperature: body.get("temperature").and_then(|t| t.as_f64()),
        top_p: body.get("top_p").and_then(|t| t.as_f64()),
        max_tokens: body.get("max_tokens").or_else(|| body.get("max_output_tokens")).and_then(|m| m.as_u64()),
        // Previously hardcoded to `false` regardless of the caller's
        // request (Audit A09). Now also drives real Responses-API SSE
        // output below when true, instead of only being forwarded to the
        // upstream provider (docs/REMEDIATION_PLAN.md Phase 2.2).
        stream,
        // The Responses API's `reasoning: {"effort": "..."}` was previously
        // dropped entirely (Audit A09) — mapped onto the same
        // `reasoning_effort` field Chat Completions uses.
        reasoning_effort: body
            .get("reasoning")
            .and_then(|r| r.get("effort"))
            .and_then(|e| e.as_str())
            .map(String::from),
        user: body.get("user").and_then(|u| u.as_str()).map(|s| s.to_string()),
        ..Default::default()
    };

    // Emit telemetry
    state.emit_telemetry(TelemetryEvent::RequestReceived {
        request_id,
        timestamp: received_at,
        client_correlation_id,
        application: None,
        task: Some("responses".to_string()),
        tags: vec!["responses".to_string()],
        requested_model: model.clone(),
        streaming: false,
        canonical_messages: request.messages.as_ref().and_then(|m| serde_json::to_value(m).ok()),
        raw_request: Some(body.clone()),
        uses_tools: false,
        uses_vision: false,
        uses_structured_output: false,
        uses_reasoning: false,
    });

    let routing_plan = match state.router.route(&request, &state.model_registry, &state.provider_registry) {
        Ok(plan) => plan,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, "routing_error", &e.to_string());
        }
    };

    // ─── Emit: RouteSelected ─────────────────────────────────────────────
    // Previously missing entirely from this handler (Audit A09) — emitted
    // even on zero-candidate routing failures (Audit CL02).
    state.emit_telemetry(TelemetryEvent::RouteSelected {
        request_id,
        timestamp: Utc::now(),
        decision: routing_plan.decision.clone(),
    });

    if routing_plan.candidates.is_empty() {
        state.emit_telemetry(TelemetryEvent::RequestCompleted {
            request_id,
            timestamp: Utc::now(),
            total_latency_us: (Utc::now() - received_at).num_microseconds().unwrap_or(0) as u64,
            total_attempts: 0,
            serving_provider: None,
            resolved_model: None,
            success: false,
        });
        return error_response(
            StatusCode::BAD_REQUEST,
            "routing_error",
            &format!(
                "No compatible providers available for model '{}': {} candidate(s) rejected",
                model,
                routing_plan.decision.candidates_rejected
            ),
        );
    }

    // ─── Build Retry / Fallback Configuration ────────────────────────────
    let retry_config = fallback::RetryConfig {
        max_attempts_per_provider: state.config.routing.max_retries_per_provider,
        fallback_on: state
            .config
            .routing
            .fallback_on
            .iter()
            .filter_map(|s| match s.as_str() {
                "timeout" => Some(fallback::FallbackTrigger::Timeout),
                "rate_limit" => Some(fallback::FallbackTrigger::RateLimit),
                "unavailable" => Some(fallback::FallbackTrigger::Unavailable),
                "server_error" => Some(fallback::FallbackTrigger::ServerError),
                "content_filter" => Some(fallback::FallbackTrigger::ContentFilter),
                _ => None,
            })
            .collect(),
        ..Default::default()
    };

    let max_retries = state.config.routing.max_retries_per_provider.max(1);
    let mut attempt_number = 0u32;
    let mut last_error: Option<String> = None;

    'candidate_loop: for candidate in &routing_plan.candidates {
        let provider = match state.provider_registry.get(&candidate.provider) {
            Some(p) => p,
            None => continue,
        };

        for attempt in 0..max_retries {
            attempt_number += 1;
            state.provider_registry.record_request_start(&candidate.provider);

            // See the identical comment in chat_completions.rs::handle —
            // this is the sole emitter of ProviderAttemptStarted so that
            // connection/HTTP failures that never reach a stream are still
            // recorded (Audit A03).
            let attempt_started_at = Utc::now();
            state.emit_telemetry(TelemetryEvent::ProviderAttemptStarted {
                request_id,
                attempt_number,
                timestamp: attempt_started_at,
                provider: candidate.provider.clone(),
                model: candidate.model.clone(),
            });

            let attempt_timeout = std::time::Duration::from_secs(state.config.routing.attempt_timeout_secs.max(1));
            let exec_result = tokio::time::timeout(attempt_timeout, provider.execute(&request, &candidate.model)).await;

            match exec_result {
                Ok(Ok(ProviderOutput::Stream(provider_stream))) => {
                    // Previously `record_request_end` was called here,
                    // before the stream was even consumed, and then called
                    // *again* below on `stream_err` or success — a genuine
                    // double-decrement of `active_requests` (Audit A13).
                    // The attempt only truly ends once the stream is fully
                    // drained, at the `stream_err` branch or the success
                    // path below.
                    // `!stream` means the adapter took its non-streaming
                    // branch and handed back a one-shot sequence of events
                    // reconstructed from a single HTTP response, not a
                    // genuine live stream — see `InstrumentedStream`'s
                    // `synthetic` field doc.
                    let instrumented = InstrumentedStream::new(
                        provider_stream,
                        request_id,
                        attempt_number,
                        state.telemetry_tx.clone(),
                        state.telemetry_health.clone(),
                        state.config.telemetry.store_stream_events,
                        !stream,
                    );

                    if stream {
                        // Real Responses-API SSE output — previously this
                        // endpoint always collected the full response
                        // server-side and returned one JSON object even when
                        // the caller requested `stream: true`
                        // (docs/REMEDIATION_PLAN.md Phase 2.2 / Audit A09).
                        // Scoped to the events this gateway's provider
                        // adapters actually produce: text deltas, a final
                        // `response.completed`/`response.failed`, and a
                        // passthrough `error` event mid-stream. Tool calls,
                        // reasoning summaries, and multi-item output are not
                        // covered — this is a documented subset, not full
                        // Responses-API streaming parity.
                        let response_id = format!("resp_{}", request_id);
                        let item_id = format!("msg_{}", Uuid::now_v7());
                        let response_model = candidate.model.clone();

                        let guard = Arc::new(super::chat_completions::StreamGuard {
                            state: state.clone(),
                            provider: candidate.provider.clone(),
                            model: candidate.model.clone(),
                            request_id,
                            received_at,
                            attempt_number,
                            completed: std::sync::atomic::AtomicBool::new(false),
                        });
                        let guard_for_events = guard.clone();
                        let guard_for_done = guard;

                        let accumulated_content = Arc::new(Mutex::new(String::new()));
                        let accumulated_for_events = accumulated_content.clone();

                        let created_event = Event::default()
                            .event("response.created")
                            .json_data(serde_json::json!({
                                "type": "response.created",
                                "response": {
                                    "id": response_id,
                                    "object": "response",
                                    "created_at": Utc::now().timestamp(),
                                    "model": response_model,
                                    "status": "in_progress",
                                    "output": [],
                                }
                            }))
                            .expect("static JSON always serializes");

                        let body_item_id = item_id.clone();
                        let body_stream = instrumented.filter_map(move |event| {
                            let guard = guard_for_events.clone();
                            let accumulated = accumulated_for_events.clone();
                            let item_id = body_item_id.clone();
                            async move {
                                if let ProviderEvent::ContentDelta { delta, .. } = &event {
                                    accumulated.lock().unwrap().push_str(delta);
                                }
                                if matches!(&event, ProviderEvent::Completed { .. }) {
                                    guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                match &event {
                                    ProviderEvent::ContentDelta { delta, .. } => Some(
                                        Event::default()
                                            .event("response.output_text.delta")
                                            .json_data(serde_json::json!({
                                                "type": "response.output_text.delta",
                                                "item_id": item_id,
                                                "output_index": 0,
                                                "content_index": 0,
                                                "delta": delta,
                                            }))
                                            .expect("static JSON always serializes"),
                                    ),
                                    ProviderEvent::Error { code, message, .. } => Some(
                                        Event::default().event("error").json_data(serde_json::json!({
                                            "type": "error",
                                            "code": code,
                                            "message": message,
                                        })).expect("static JSON always serializes"),
                                    ),
                                    _ => None,
                                }
                                .map(Ok::<_, std::convert::Infallible>)
                            }
                        });

                        let final_response_id = response_id.clone();
                        let final_item_id = item_id;
                        let final_model = candidate.model.clone();
                        let terminal_stream = futures::stream::once(async move {
                            let final_event = if guard_for_done
                                .completed
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                let content = accumulated_content.lock().unwrap().clone();
                                Event::default()
                                    .event("response.completed")
                                    .json_data(serde_json::json!({
                                        "type": "response.completed",
                                        "response": {
                                            "id": final_response_id,
                                            "object": "response",
                                            "created_at": Utc::now().timestamp(),
                                            "model": final_model,
                                            "status": "completed",
                                            "output": [{
                                                "type": "message",
                                                "id": final_item_id,
                                                "role": "assistant",
                                                "content": [{ "type": "output_text", "text": content }],
                                            }],
                                        }
                                    }))
                                    .expect("static JSON always serializes")
                            } else {
                                // A mid-stream error already reached the
                                // client as its own `error` event above —
                                // this tells it the response as a whole
                                // never reached a genuine completion,
                                // instead of silently implying success.
                                Event::default()
                                    .event("response.failed")
                                    .json_data(serde_json::json!({
                                        "type": "response.failed",
                                        "response": { "id": final_response_id, "status": "failed" }
                                    }))
                                    .expect("static JSON always serializes")
                            };
                            // Keep the guard alive through the terminal
                            // event; its Drop impl emits RequestCompleted
                            // reading whatever `completed` was actually set.
                            drop(guard_for_done);
                            Ok::<_, std::convert::Infallible>(final_event)
                        });

                        let sse_stream = futures::stream::once(async move {
                            Ok::<_, std::convert::Infallible>(created_event)
                        })
                        .chain(body_stream)
                        .chain(terminal_stream);

                        return Sse::new(sse_stream)
                            .keep_alive(KeepAlive::default())
                            .into_response();
                    }

                    let mut content = String::new();
                    let mut usage = None;
                    let mut pinned = std::pin::pin!(instrumented);

                    let mut stream_err = None;
                    while let Some(event) = pinned.next().await {
                        match event {
                            ProviderEvent::ContentDelta { delta, .. } => {
                                content.push_str(&delta);
                            }
                            ProviderEvent::UsageUpdate {
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                                ..
                            } => {
                                // The Responses API's own field names are
                                // `input_tokens`/`output_tokens`, not Chat
                                // Completions' `prompt_tokens`/
                                // `completion_tokens` — this previously
                                // returned the wrong names for this endpoint
                                // (Audit A09).
                                usage = Some(serde_json::json!({
                                    "input_tokens": prompt_tokens.unwrap_or(0),
                                    "output_tokens": completion_tokens.unwrap_or(0),
                                    "total_tokens": total_tokens.unwrap_or(0),
                                }));
                            }
                            ProviderEvent::Error { code, message, status_code, retryable, .. } => {
                                stream_err = Some((code, message, status_code, retryable));
                                break;
                            }
                            _ => {}
                        }
                    }

                    if let Some((code, message, status_code, retryable)) = stream_err {
                        state.provider_registry.record_request_end(&candidate.provider, true);
                        last_error = Some(message.clone());

                        let should_fb = fallback::should_fallback(&retry_config, &code, status_code);

                        if retryable && attempt + 1 < max_retries {
                            tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                            continue;
                        }

                        if !should_fb {
                            return error_response(
                                StatusCode::from_u16(status_code.unwrap_or(500))
                                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                                &code,
                                &message,
                            );
                        }
                        continue 'candidate_loop;
                    }

                    let response = serde_json::json!({
                        "id": format!("resp_{}", request_id),
                        "object": "response",
                        "created_at": Utc::now().timestamp(),
                        "model": candidate.model,
                        "status": "completed",
                        "output": [
                            {
                                "type": "message",
                                "id": format!("msg_{}", Uuid::now_v7()),
                                "role": "assistant",
                                "content": [
                                    {
                                        "type": "output_text",
                                        "text": content
                                    }
                                ]
                            }
                        ],
                        "usage": usage,
                    });

                    state.provider_registry.record_request_end(&candidate.provider, false);
                    // Previously this endpoint never fed inference latency
                    // into routing/`/metrics` at all (Audit A13).
                    state.provider_registry.record_latency(
                        &candidate.provider,
                        (Utc::now() - received_at).num_milliseconds().max(0) as u64,
                    );

                    state.emit_telemetry(TelemetryEvent::RequestCompleted {
                        request_id,
                        timestamp: Utc::now(),
                        total_latency_us: (Utc::now() - received_at).num_microseconds().unwrap_or(0) as u64,
                        total_attempts: attempt_number,
                        serving_provider: Some(candidate.provider.clone()),
                        resolved_model: Some(candidate.model.clone()),
                        success: true,
                    });

                    return Json(response).into_response();
                }
                Ok(Ok(ProviderOutput::Complete(ProviderEvent::Error { code, message, status_code, retryable, .. }))) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    last_error = Some(message.clone());

                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: super::chat_completions::attempt_status_for_code(&code),
                        finish_reason: None,
                        provider_response_id: None,
                        upstream_provider: None,
                        duration_us: (Utc::now() - attempt_started_at).num_microseconds().unwrap_or(0) as u64,
                        error_code: Some(code.clone()),
                        error_message: Some(message.clone()),
                    });

                    let should_fb = fallback::should_fallback(&retry_config, &code, status_code);

                    if retryable && attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    if !should_fb {
                        return error_response(
                            StatusCode::from_u16(status_code.unwrap_or(500))
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                            &code,
                            &message,
                        );
                    }
                    continue 'candidate_loop;
                }
                Ok(Ok(_)) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: AttemptStatus::Failed,
                        finish_reason: None,
                        provider_response_id: None,
                        upstream_provider: None,
                        duration_us: (Utc::now() - attempt_started_at).num_microseconds().unwrap_or(0) as u64,
                        error_code: Some("unexpected_output".to_string()),
                        error_message: Some("Unexpected provider output".to_string()),
                    });
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "Unexpected provider output",
                    );
                }
                Ok(Err(e)) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    let code = super::chat_completions::classify_execution_error(&e);
                    last_error = Some(e.to_string());

                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: AttemptStatus::Failed,
                        finish_reason: None,
                        provider_response_id: None,
                        upstream_provider: None,
                        duration_us: (Utc::now() - attempt_started_at).num_microseconds().unwrap_or(0) as u64,
                        error_code: Some(code.to_string()),
                        error_message: last_error.clone(),
                    });

                    if attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    let should_fb = fallback::should_fallback(&retry_config, code, None);
                    if !should_fb {
                        return error_response(
                            StatusCode::BAD_GATEWAY,
                            code,
                            &last_error.clone().unwrap_or_default(),
                        );
                    }
                    continue 'candidate_loop;
                }
                Err(_elapsed) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    let err_msg = format!("Attempt timed out after {}s", state.config.routing.attempt_timeout_secs);
                    last_error = Some(err_msg.clone());

                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: AttemptStatus::Timeout,
                        finish_reason: None,
                        provider_response_id: None,
                        upstream_provider: None,
                        duration_us: (Utc::now() - attempt_started_at).num_microseconds().unwrap_or(0) as u64,
                        error_code: Some("timeout".to_string()),
                        error_message: Some(err_msg.clone()),
                    });

                    let should_fb = fallback::should_fallback(&retry_config, "timeout", Some(504));
                    if attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    if !should_fb {
                        return error_response(
                            StatusCode::GATEWAY_TIMEOUT,
                            "gateway_timeout",
                            &err_msg,
                        );
                    }
                    continue 'candidate_loop;
                }
            }
        }
    }

    state.emit_telemetry(TelemetryEvent::RequestCompleted {
        request_id,
        timestamp: Utc::now(),
        total_latency_us: (Utc::now() - received_at).num_microseconds().unwrap_or(0) as u64,
        total_attempts: attempt_number,
        serving_provider: None,
        resolved_model: None,
        success: false,
    });

    error_response(
        StatusCode::BAD_GATEWAY,
        "responses_failed",
        &last_error.unwrap_or_else(|| "All providers failed for responses request".to_string()),
    )
}

/// Parse a standard Responses API object-form input item
/// (`{"role": "user", "content": "..."}` or with `content` as an array of
/// `{"type": "input_text", "text": "..."}`-shaped parts) into a canonical
/// `Message`. Returns `None` for anything else (e.g. tool-result items),
/// which remain unsupported rather than silently misinterpreted.
fn parse_input_message_object(item: &serde_json::Value) -> Option<Message> {
    let obj = item.as_object()?;
    let role_str = obj.get("role")?.as_str()?;
    let role = match role_str {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        "developer" => Role::Developer,
        _ => return None,
    };

    let content_val = obj.get("content")?;
    let text = if let Some(s) = content_val.as_str() {
        s.to_string()
    } else if let Some(parts) = content_val.as_array() {
        parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("")
    } else {
        return None;
    };

    if text.is_empty() {
        return None;
    }

    Some(Message {
        role,
        content: Some(MessageContent::Text(text)),
        name: None,
        tool_call_id: None,
        tool_calls: None,
    })
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": code,
            "code": code,
        }
    });
    (status, Json(body)).into_response()
}
