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
use crate::telemetry::events::{AttemptStatus, TelemetryEvent};
use crate::telemetry::stream_instrumentation::InstrumentedStream;
use crate::types::request::GatewayRequest;

// `pub(crate)` so `responses.rs` can reuse it for its own SSE streaming
// path instead of duplicating the same active-request/latency/
// RequestCompleted bookkeeping (docs/REMEDIATION_PLAN.md Phase 2.2).
pub(crate) struct StreamGuard {
    pub(crate) state: Arc<AppState>,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) request_id: Uuid,
    pub(crate) received_at: chrono::DateTime<Utc>,
    pub(crate) attempt_number: u32,
    pub(crate) completed: std::sync::atomic::AtomicBool,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let was_completed = self.completed.load(std::sync::atomic::Ordering::Relaxed);
        self.state.provider_registry.record_request_end(&self.provider, !was_completed);
        let duration_us = (Utc::now() - self.received_at).num_microseconds().unwrap_or(0) as u64;
        if was_completed {
            // Previously the streaming (SSE) success path never fed
            // `lowest_latency` routing or `/metrics` inference latency at
            // all — only health checks and embeddings did (Audit A13).
            self.state
                .provider_registry
                .record_latency(&self.provider, duration_us / 1000);
        }
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
    body: axum::body::Bytes,
) -> Response {
    state.record_gateway_request();

    // Parse into a raw `Value` first and keep it verbatim for telemetry,
    // then deserialize the canonical `GatewayRequest` from that same value.
    // Previously the body was deserialized directly via the `Json<T>`
    // extractor and the *typed* struct was re-serialized as "raw_request" —
    // any field not in `GatewayRequest` (e.g. a client testing a new
    // provider-side field) was silently erased before it ever reached
    // storage (Audit A05).
    let raw_value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("Invalid JSON body: {e}"),
            );
        }
    };
    let request: GatewayRequest = match serde_json::from_value(raw_value.clone()) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("Invalid request: {e}"),
            );
        }
    };

    // `request_id` is always gateway-generated (see GatewayRequest's default)
    // and is never overwritten by a client-supplied header — an inbound
    // `x-request-id` is preserved only as an opaque correlation ID, never as
    // the database primary key, so a client reusing an ID across distinct
    // requests cannot overwrite another request's stored data.
    let client_correlation_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let request_id = request.request_id;
    let received_at = Utc::now();
    let uses_vision = state.router.requires_vision(&request);

    // ─── Emit: RequestReceived ───────────────────────────────────────────
    state.emit_telemetry(TelemetryEvent::RequestReceived {
        request_id,
        timestamp: received_at,
        client_correlation_id,
        application: request.metadata.as_ref().and_then(|m| m.application.clone()),
        task: request.metadata.as_ref().and_then(|m| m.task.clone()),
        tags: request.metadata.as_ref().map(|m| m.tags.clone()).unwrap_or_default(),
        requested_model: request.model.clone(),
        streaming: request.stream,
        canonical_messages: request.messages.as_ref().and_then(|m| serde_json::to_value(m).ok()),
        raw_request: Some(raw_value),
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
    // Emitted even when routing found zero viable candidates, so "why
    // nothing was selected" is always persisted (docs/REMEDIATION_PLAN.md
    // Phase 1.4 / Audit CL02).
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
                request.model,
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
            None => {
                last_error = Some(format!("Provider '{}' not found", candidate.provider));
                continue;
            }
        };

        for attempt in 0..max_retries {
            attempt_number += 1;
            state.provider_registry.record_request_start(&candidate.provider);

            // Emitted before calling the provider, so connection failures,
            // HTTP errors, and timeouts — none of which ever reach
            // InstrumentedStream — still produce a recorded attempt
            // (Audit A03). This is now the sole emitter of
            // ProviderAttemptStarted; InstrumentedStream no longer emits it
            // for the streaming path to avoid double-recording the same
            // attempt.
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
                Ok(Ok(ProviderOutput::Stream(stream))) => {
                    // Wrap in instrumented stream. `!request.stream` means
                    // the adapter took its non-streaming branch and handed
                    // back a one-shot sequence of events reconstructed from
                    // a single HTTP response, not a genuine live stream —
                    // timing derived from it is fabricated (see
                    // `InstrumentedStream`'s `synthetic` field doc).
                    let instrumented = InstrumentedStream::new(
                        stream,
                        request_id,
                        attempt_number,
                        state.telemetry_tx.clone(),
                        state.telemetry_health.clone(),
                        state.config.telemetry.store_stream_events,
                        !request.stream,
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
                                    // Only a genuine `Completed` event marks
                                    // this attempt as successful — the
                                    // `[DONE]` sentinel below is always sent
                                    // (clients need it to terminate the SSE
                                    // stream) but must not itself flip
                                    // `success` to true after a mid-stream
                                    // error (Audit A04).
                                    if matches!(&event, ProviderEvent::Completed { .. }) {
                                        guard.completed.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    event_to_sse(&event, &chunk_id, created_ts, &model_str).map(|data| {
                                        Ok::<_, std::convert::Infallible>(Event::default().data(data))
                                    })
                                }
                            })
                            .chain(futures::stream::once(async move {
                                // Keep the guard alive through the final
                                // chunk; its Drop impl (which emits
                                // RequestCompleted) reads whatever
                                // `completed` was actually set to above —
                                // sending [DONE] no longer forces it true.
                                drop(guard_for_done);
                                Ok::<_, std::convert::Infallible>(Event::default().data("[DONE]"))
                            }));

                        return Sse::new(sse_stream)
                            .keep_alive(KeepAlive::default())
                            .into_response();
                    } else {
                        // Collect stream into a single response. Keyed by
                        // choice `index` so an `n > 1` request's other
                        // choices reach the client instead of being merged
                        // into one (docs/REMEDIATION_PLAN.md Phase 2.3 /
                        // Audit A08) — the common `n == 1` case is just a
                        // one-entry map.
                        #[derive(Default)]
                        struct ChoiceAccumulator {
                            content: String,
                            reasoning: String,
                            tool_calls: std::collections::BTreeMap<u32, (Option<String>, Option<String>, String)>,
                        }
                        let mut choices: std::collections::BTreeMap<u32, ChoiceAccumulator> =
                            std::collections::BTreeMap::new();
                        let mut usage = None;
                        let mut finish_reason = None;
                        let mut stream_error = None;
                        let mut pinned = std::pin::pin!(instrumented);

                        while let Some(event) = pinned.next().await {
                            match event {
                                ProviderEvent::ContentDelta { index, delta, .. } => {
                                    choices.entry(index).or_default().content.push_str(&delta);
                                }
                                ProviderEvent::ReasoningDelta { delta, .. } => {
                                    // Reasoning has no per-choice index in
                                    // the event model — always attributed to
                                    // choice 0. See the matching note in
                                    // openai_compatible.rs.
                                    choices.entry(0).or_default().reasoning.push_str(&delta);
                                }
                                ProviderEvent::ToolCallDelta {
                                    index,
                                    tool_call_index,
                                    id,
                                    name,
                                    arguments,
                                    ..
                                } => {
                                    let entry = choices
                                        .entry(index)
                                        .or_default()
                                        .tool_calls
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
                                    cached_tokens,
                                    reasoning_tokens,
                                    ..
                                } => {
                                    // Previously dropped cached/reasoning
                                    // token detail even when the provider
                                    // reported it (Audit A08).
                                    usage = Some(serde_json::json!({
                                        "prompt_tokens": prompt_tokens.unwrap_or(0),
                                        "completion_tokens": completion_tokens.unwrap_or(0),
                                        "total_tokens": total_tokens.unwrap_or(0),
                                        "prompt_tokens_details": { "cached_tokens": cached_tokens.unwrap_or(0) },
                                        "completion_tokens_details": { "reasoning_tokens": reasoning_tokens.unwrap_or(0) },
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

                        state.provider_registry.record_request_end(&candidate.provider, false);
                        state.provider_registry.record_latency(
                            &candidate.provider,
                            (Utc::now() - received_at).num_milliseconds().max(0) as u64,
                        );

                        // At least one choice must always be present, even
                        // if the provider returned genuinely empty content
                        // (matches the prior single-choice behavior).
                        if choices.is_empty() {
                            choices.insert(0, ChoiceAccumulator::default());
                        }

                        // Every choice shares the attempt's single
                        // `finish_reason` — the event model only carries one
                        // per attempt (see the Completed event), so distinct
                        // per-choice finish reasons for `n > 1` are not
                        // currently distinguishable. Documented scoping, not
                        // an oversight.
                        let shared_finish_reason = finish_reason.unwrap_or_else(|| "stop".to_string());

                        let choices_json: Vec<serde_json::Value> = choices
                            .into_iter()
                            .map(|(index, acc)| {
                                let tool_calls_json: Vec<serde_json::Value> = acc
                                    .tool_calls
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
                                    "content": if acc.content.is_empty() && !tool_calls_json.is_empty() {
                                        serde_json::Value::Null
                                    } else {
                                        serde_json::Value::String(acc.content)
                                    },
                                });

                                if !acc.reasoning.is_empty() {
                                    message_obj["reasoning_content"] = serde_json::Value::String(acc.reasoning);
                                }
                                if !tool_calls_json.is_empty() {
                                    message_obj["tool_calls"] = serde_json::Value::Array(tool_calls_json);
                                }

                                serde_json::json!({
                                    "index": index,
                                    "message": message_obj,
                                    "finish_reason": shared_finish_reason,
                                })
                            })
                            .collect();

                        let response = serde_json::json!({
                            "id": format!("chatcmpl-{}", request_id),
                            "object": "chat.completion",
                            "created": Utc::now().timestamp(),
                            "model": candidate.model,
                            "choices": choices_json,
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

                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: attempt_status_for_code(&code),
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

                    // `should_fb` (derived from `routing.fallback_on`) is the
                    // sole authority for whether we move to the next
                    // candidate once retries are exhausted — previously
                    // `!should_fb && !retryable` meant a retryable error with
                    // `fallback_on: []` still silently fell back to the next
                    // provider (Audit A12).
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
                    // The adapter returned a `ProviderOutput` variant this
                    // loop doesn't otherwise handle — an adapter bug, not a
                    // normal fallback-trigger category, so it is never
                    // silently retried onto another provider (Audit A12).
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
                    let code = classify_execution_error(&e);
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
        // Previously discarded entirely (Audit A08) — usage is one of the
        // explicit metrics the gateway exists to capture, and OpenAI's own
        // `stream_options: {"include_usage": true}` convention delivers it
        // as a final chunk with empty choices, which we mirror here.
        ProviderEvent::UsageUpdate {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            reasoning_tokens,
            ..
        } => {
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [],
                "usage": {
                    "prompt_tokens": prompt_tokens.unwrap_or(0),
                    "completion_tokens": completion_tokens.unwrap_or(0),
                    "total_tokens": total_tokens.unwrap_or(0),
                    "prompt_tokens_details": { "cached_tokens": cached_tokens.unwrap_or(0) },
                    "completion_tokens_details": { "reasoning_tokens": reasoning_tokens.unwrap_or(0) },
                }
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        // A mid-stream provider error was previously dropped entirely here
        // (the catch-all `_ => None` below), so a client that had already
        // received a partial response saw it end silently with only
        // `[DONE]` and no indication anything went wrong (Audit A04).
        // OpenAI's streaming protocol has no single standardized error
        // chunk shape, so we emit the same `{"error": {...}}` envelope used
        // for non-streaming failures, as its own SSE data line before the
        // stream's trailing `[DONE]`.
        ProviderEvent::Error { code, message, .. } => {
            let data = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "error": {
                    "message": message,
                    "type": code,
                    "code": code,
                }
            });
            Some(serde_json::to_string(&data).unwrap_or_default())
        }
        _ => None,
    }
}

/// Map a fallback-trigger-style error code to an `AttemptStatus` for
/// telemetry, mirroring `InstrumentedStream`'s classification so attempts
/// recorded via the streaming and non-streaming paths use consistent status
/// values.
pub fn attempt_status_for_code(code: &str) -> AttemptStatus {
    if code == "rate_limit" || code == "429" {
        AttemptStatus::RateLimited
    } else if code == "timeout" {
        AttemptStatus::Timeout
    } else {
        AttemptStatus::Failed
    }
}

/// Classify a generic provider execution error (connection failures, DNS
/// errors, etc. surfaced as `anyhow::Error` before any `ProviderEvent` could
/// be constructed) into a fallback-trigger code, so `fallback_on` config is
/// authoritative here too rather than these errors always falling back
/// unconditionally (Audit A12).
pub fn classify_execution_error(e: &anyhow::Error) -> &'static str {
    if let Some(reqwest_err) = e.downcast_ref::<reqwest::Error>() {
        if reqwest_err.is_timeout() {
            return "timeout";
        }
        if reqwest_err.is_connect() {
            return "unavailable";
        }
    }
    "unavailable"
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
