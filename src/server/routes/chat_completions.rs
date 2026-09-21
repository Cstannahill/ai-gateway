use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use futures::StreamExt;
use uuid::Uuid;

use crate::providers::ProviderOutput;
use crate::providers::events::ProviderEvent;
use crate::routing::fallback;
use crate::server::AppState;
use crate::telemetry::events::TelemetryEvent;
use crate::telemetry::stream_instrumentation::InstrumentedStream;
use crate::types::request::GatewayRequest;

struct StreamGuard {
    state: Arc<AppState>,
    provider: String,
    model: String,
    request_id: Uuid,
    received_at: chrono::DateTime<Utc>,
    attempt_number: u32,
    completed: std::sync::atomic::AtomicBool,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let was_completed = self.completed.load(std::sync::atomic::Ordering::Relaxed);
        self.state.provider_registry.record_request_end(&self.provider, !was_completed);
        let duration_us = (Utc::now() - self.received_at).num_microseconds().unwrap_or(0) as u64;
        self.state.emit_telemetry(TelemetryEvent::RequestCompleted {
            request_id: self.request_id,
            timestamp: Utc::now(),
            total_latency_us: duration_us,
            total_attempts: self.attempt_number,
            serving_provider: Some(self.provider.clone()),
            resolved_model: Some(self.model.clone()),
            success: was_completed,
        });
    }
}

/// POST /v1/chat/completions
///
/// OpenAI-compatible chat completions endpoint.
/// Routes to a provider, instruments the stream, and returns SSE or JSON.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(mut request): Json<GatewayRequest>,
) -> Response {
    if let Some(header_id) = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        request.request_id = header_id;
    }
    let request_id = request.request_id;
    let received_at = Utc::now();
    let uses_vision = state.router.requires_vision(&request);

    // ─── Emit: RequestReceived ───────────────────────────────────────────
    state.emit_telemetry(TelemetryEvent::RequestReceived {
        request_id,
        timestamp: received_at,
        application: request.metadata.as_ref().and_then(|m| m.application.clone()),
        task: request.metadata.as_ref().and_then(|m| m.task.clone()),
        tags: request.metadata.as_ref().map(|m| m.tags.clone()).unwrap_or_default(),
        requested_model: request.model.clone(),
        streaming: request.stream,
        canonical_messages: request.messages.as_ref().and_then(|m| serde_json::to_value(m).ok()),
        raw_request: serde_json::to_value(&request).ok(),
        uses_tools: request.tools.is_some(),
        uses_vision,
        uses_structured_output: request.response_format.is_some(),
        uses_reasoning: request.reasoning.as_ref().is_some_and(|r| r.enabled),
    });

    // ─── Route the request ───────────────────────────────────────────────
    let routing_plan = match state.router.route(
        &request,
        &state.model_registry,
        &state.provider_registry,
    ) {
        Ok(plan) => plan,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "routing_error",
                &e.to_string(),
            );
        }
    };

    // ─── Emit: RouteSelected ─────────────────────────────────────────────
    state.emit_telemetry(TelemetryEvent::RouteSelected {
        request_id,
        timestamp: Utc::now(),
        decision: routing_plan.decision.clone(),
    });

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
            None => {
                last_error = Some(format!("Provider '{}' not found", candidate.provider));
                continue;
            }
        };

        for attempt in 0..max_retries {
            attempt_number += 1;
            state.provider_registry.record_request_start(&candidate.provider);

            let attempt_timeout = std::time::Duration::from_secs(state.config.routing.attempt_timeout_secs.max(1));
            let exec_result = tokio::time::timeout(attempt_timeout, provider.execute(&request, &candidate.model)).await;

            match exec_result {
                Ok(Ok(ProviderOutput::Stream(stream))) => {
                    // Wrap in instrumented stream.
                    let instrumented = InstrumentedStream::new(
                        stream,
                        request_id,
                        attempt_number,
                        state.telemetry_tx.clone(),
                        state.config.telemetry.store_stream_events,
                    );

                    if request.stream {
                        let chunk_id = format!("chatcmpl-{}", request_id);
                        let created_ts = received_at.timestamp();
                        let candidate_model = candidate.model.clone();

                        let guard = Arc::new(StreamGuard {
                            state: state.clone(),
                            provider: candidate.provider.clone(),
                            model: candidate.model.clone(),
                            request_id,
                            received_at,
                            attempt_number,
                            completed: std::sync::atomic::AtomicBool::new(false),
                        });
                        let guard_for_events = guard.clone();
                        let guard_for_done = guard.clone();

                        let sse_stream = instrumented
                            .filter_map(move |event| {
                                let chunk_id = chunk_id.clone();
                                let model_str = candidate_model.clone();
                                let guard = guard_for_events.clone();
                                async move {
                                    if matches!(&event, ProviderEvent::Completed { .. }) {
                                        guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    event_to_sse(&event, &chunk_id, created_ts, &model_str).map(|data| {
                                        Ok::<_, std::convert::Infallible>(Event::default().data(data))
                                    })
                                }
                            })
                            .chain(futures::stream::once(async move {
                                guard_for_done.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                                Ok::<_, std::convert::Infallible>(Event::default().data("[DONE]"))
                            }));

                        return Sse::new(sse_stream)
                            .keep_alive(KeepAlive::default())
                            .into_response();
                    } else {
                        // Collect stream into a single response.
                        let mut content = String::new();
                        let mut reasoning = String::new();
                        let mut tool_calls_map: std::collections::BTreeMap<u32, (Option<String>, Option<String>, String)> =
                            std::collections::BTreeMap::new();
                        let mut usage = None;
                        let mut finish_reason = None;
                        let mut stream_error = None;
                        let mut pinned = std::pin::pin!(instrumented);

                        while let Some(event) = pinned.next().await {
                            match event {
                                ProviderEvent::ContentDelta { delta, .. } => {
                                    content.push_str(&delta);
                                }
                                ProviderEvent::ReasoningDelta { delta, .. } => {
                                    reasoning.push_str(&delta);
                                }
                                ProviderEvent::ToolCallDelta {
                                    tool_call_index,
                                    id,
                                    name,
                                    arguments,
                                    ..
                                } => {
                                    let entry = tool_calls_map
                                        .entry(tool_call_index)
                                        .or_insert_with(|| (None, None, String::new()));
                                    if let Some(id_str) = id {
                                        entry.0 = Some(id_str);
                                    }
                                    if let Some(name_str) = name {
                                        entry.1 = Some(name_str);
                                    }
                                    if let Some(args_str) = arguments {
                                        entry.2.push_str(&args_str);
                                    }
                                }
                                ProviderEvent::UsageUpdate {
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                    ..
                                } => {
                                    usage = Some(serde_json::json!({
                                        "prompt_tokens": prompt_tokens.unwrap_or(0),
                                        "completion_tokens": completion_tokens.unwrap_or(0),
                                        "total_tokens": total_tokens.unwrap_or(0),
                                    }));
                                }
                                ProviderEvent::Completed { finish_reason: fr, .. } => {
                                    finish_reason = fr;
                                }
                                ProviderEvent::Error { code, message, status_code, retryable, .. } => {
                                    stream_error = Some((code, message, status_code, retryable));
                                    break;
                                }
                                _ => {}
                            }
                        }

                        if let Some((code, message, status_code, retryable)) = stream_error {
                            state.provider_registry.record_request_end(&candidate.provider, true);
                            last_error = Some(message.clone());

                            let should_fb = fallback::should_fallback(&retry_config, &code, status_code);

                            if retryable && attempt + 1 < max_retries {
                                tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                                continue;
                            }

                            if !should_fb && !retryable {
                                return error_response(
                                    StatusCode::from_u16(status_code.unwrap_or(500))
                                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                                    &code,
                                    &message,
                                );
                            }
                            continue 'candidate_loop;
                        }

                        state.provider_registry.record_request_end(&candidate.provider, false);

                        let tool_calls_json: Vec<serde_json::Value> = tool_calls_map
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

                        let mut message_obj = serde_json::json!({
                            "role": "assistant",
                            "content": if content.is_empty() && !tool_calls_json.is_empty() {
                                serde_json::Value::Null
                            } else {
                                serde_json::Value::String(content)
                            },
                        });

                        if !reasoning.is_empty() {
                            message_obj["reasoning_content"] = serde_json::Value::String(reasoning);
                        }
                        if !tool_calls_json.is_empty() {
                            message_obj["tool_calls"] = serde_json::Value::Array(tool_calls_json);
                        }

                        let response = serde_json::json!({
                            "id": format!("chatcmpl-{}", request_id),
                            "object": "chat.completion",
                            "created": Utc::now().timestamp(),
                            "model": candidate.model,
                            "choices": [{
                                "index": 0,
                                "message": message_obj,
                                "finish_reason": finish_reason.or_else(|| Some("stop".to_string())),
                            }],
                            "usage": usage,
                        });

                        // Emit RequestCompleted.
                        state.emit_telemetry(TelemetryEvent::RequestCompleted {
                            request_id,
                            timestamp: Utc::now(),
                            total_latency_us: (Utc::now() - received_at)
                                .num_microseconds()
                                .unwrap_or(0) as u64,
                            total_attempts: attempt_number,
                            serving_provider: Some(candidate.provider.clone()),
                            resolved_model: Some(candidate.model.clone()),
                            success: true,
                        });

                        return Json(response).into_response();
                    }
                }

                Ok(Ok(ProviderOutput::Complete(ProviderEvent::Error {
                    code,
                    message,
                    status_code,
                    retryable,
                    ..
                }))) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    last_error = Some(message.clone());

                    let should_fb = fallback::should_fallback(&retry_config, &code, status_code);

                    if retryable && attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    if !should_fb && !retryable {
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
                    state.provider_registry.record_request_end(&candidate.provider, false);
                    last_error = Some("Unexpected provider output".to_string());
                    continue 'candidate_loop;
                }

                Ok(Err(e)) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    last_error = Some(e.to_string());
                    if attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }
                    continue 'candidate_loop;
                }

                Err(_elapsed) => {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    let err_msg = format!("Attempt timed out after {}s", state.config.routing.attempt_timeout_secs);
                    last_error = Some(err_msg.clone());

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

    // All candidates exhausted.
    state.emit_telemetry(TelemetryEvent::RequestCompleted {
        request_id,
        timestamp: Utc::now(),
        total_latency_us: (Utc::now() - received_at)
            .num_microseconds()
            .unwrap_or(0) as u64,
        total_attempts: attempt_number,
        serving_provider: None,
        resolved_model: None,
        success: false,
    });

    error_response(
        StatusCode::BAD_GATEWAY,
        "all_providers_failed",
        &last_error.unwrap_or_else(|| "No providers available".to_string()),
    )
}

/// Convert a ProviderEvent to an SSE data string compliant with OpenAI specification.
pub fn event_to_sse(event: &ProviderEvent, id: &str, created: i64, model: &str) -> Option<String> {
    match event {
        ProviderEvent::ContentDelta { index, delta, .. } => {
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": index,
                    "delta": { "content": delta },
                    "finish_reason": null,
                }]
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        ProviderEvent::ReasoningDelta { delta, .. } => {
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": delta },
                    "finish_reason": null,
                }]
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        ProviderEvent::ToolCallDelta {
            index,
            tool_call_index,
            id: tc_id,
            name,
            arguments,
            ..
        } => {
            let mut tc = serde_json::json!({ "index": tool_call_index });
            if let Some(id_str) = tc_id {
                tc["id"] = serde_json::json!(id_str);
            }
            if let Some(name_str) = name {
                tc["type"] = serde_json::json!("function");
                tc["function"] = serde_json::json!({ "name": name_str });
            }
            if let Some(args) = arguments {
                tc["function"]["arguments"] = serde_json::json!(args);
            }
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": index,
                    "delta": { "tool_calls": [tc] },
                    "finish_reason": null,
                }]
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        ProviderEvent::Completed { finish_reason, .. } => {
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": finish_reason.as_deref().unwrap_or("stop"),
                }]
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        _ => None,
    }
}

/// Build an OpenAI-compatible error response.
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
