use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use uuid::Uuid;

use crate::routing::fallback;
use crate::server::AppState;
use crate::telemetry::events::{AttemptStatus, TelemetryEvent};
use crate::types::Usage;

/// POST /v1/embeddings
///
/// OpenAI-compatible embeddings endpoint.
/// Resolves the requested model, routes to an available embeddings-capable provider,
/// and returns the embeddings response.
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

    let input = match body.get("input") {
        Some(inp) => inp.clone(),
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "missing_input",
                "Request must include an 'input' parameter",
            );
        }
    };

    // Emit telemetry
    state.emit_telemetry(TelemetryEvent::RequestReceived {
        request_id,
        timestamp: received_at,
        client_correlation_id,
        application: None,
        task: Some("embeddings".to_string()),
        tags: vec!["embeddings".to_string()],
        requested_model: model.clone(),
        streaming: false,
        canonical_messages: None,
        raw_request: Some(body.clone()),
        uses_tools: false,
        uses_vision: false,
        uses_structured_output: false,
        uses_reasoning: false,
    });

    // Resolve candidates for this model
    let resolved = match state.model_registry.resolve(&model) {
        Ok(res) => res,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "model_not_found",
                &e.to_string(),
            );
        }
    };

    // Same retry/fallback configuration as chat completions & responses, so
    // `routing.fallback_on`/`max_retries_per_provider` are authoritative
    // here too instead of embeddings having its own untimed, unretried,
    // untelemetered execution path (docs/REMEDIATION_PLAN.md Phase 1.1/2.4 —
    // Audit A14).
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
    let attempt_timeout = std::time::Duration::from_secs(state.config.routing.attempt_timeout_secs.max(1));

    let mut last_error = None;
    let mut attempt_number = 0u32;

    'candidate_loop: for candidate in &resolved.candidates {
        if !state.provider_registry.is_available(&candidate.provider) {
            continue;
        }

        let provider = match state.provider_registry.get(&candidate.provider) {
            Some(p) => p,
            None => continue,
        };

        if !provider.capabilities().embeddings {
            continue;
        }

        for attempt in 0..max_retries {
            attempt_number += 1;
            state.provider_registry.record_request_start(&candidate.provider);

            let attempt_started_at = Utc::now();
            state.emit_telemetry(TelemetryEvent::ProviderAttemptStarted {
                request_id,
                attempt_number,
                timestamp: attempt_started_at,
                provider: candidate.provider.clone(),
                model: candidate.model.clone(),
            });

            let exec_result = tokio::time::timeout(
                attempt_timeout,
                provider.embeddings(&candidate.model, &input),
            )
            .await;

            let duration_us = (Utc::now() - attempt_started_at).num_microseconds().unwrap_or(0) as u64;

            match exec_result {
                Ok(Ok(result)) => {
                    let latency_ms = (Utc::now() - received_at).num_milliseconds().max(0) as u64;
                    state.provider_registry.record_request_end(&candidate.provider, false);
                    state.provider_registry.record_latency(&candidate.provider, latency_ms);

                    state.emit_telemetry(TelemetryEvent::ProviderAttemptCompleted {
                        request_id,
                        attempt_number,
                        timestamp: Utc::now(),
                        status: AttemptStatus::Success,
                        finish_reason: None,
                        provider_response_id: result.get("id").and_then(|v| v.as_str()).map(String::from),
                        upstream_provider: None,
                        duration_us,
                        error_code: None,
                        error_message: None,
                    });

                    if let Some(usage) = result.get("usage") {
                        state.emit_telemetry(TelemetryEvent::UsageRecorded {
                            request_id,
                            attempt_number,
                            timestamp: Utc::now(),
                            usage: Usage {
                                prompt_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                completion_tokens: 0,
                                total_tokens: usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                cached_tokens: None,
                                reasoning_tokens: None,
                                raw_provider_usage: Some(usage.clone()),
                            },
                            // Embeddings has no streaming concept — always a
                            // single non-streaming response body. See
                            // docs/frontend/API_GAPS.md item 2.
                            source: "non_streaming".to_string(),
                            kind: "cumulative".to_string(),
                        });
                    }

                    state.emit_telemetry(TelemetryEvent::RequestCompleted {
                        request_id,
                        timestamp: Utc::now(),
                        total_latency_us: (Utc::now() - received_at).num_microseconds().unwrap_or(0) as u64,
                        total_attempts: attempt_number,
                        serving_provider: Some(candidate.provider.clone()),
                        resolved_model: Some(candidate.model.clone()),
                        success: true,
                    });

                    return Json(result).into_response();
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
                        duration_us,
                        error_code: Some(code.to_string()),
                        error_message: last_error.clone(),
                    });

                    if attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    if !fallback::should_fallback(&retry_config, code, None) {
                        break 'candidate_loop;
                    }
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
                        duration_us,
                        error_code: Some("timeout".to_string()),
                        error_message: Some(err_msg),
                    });

                    if attempt + 1 < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
                        continue;
                    }

                    if !fallback::should_fallback(&retry_config, "timeout", Some(504)) {
                        break 'candidate_loop;
                    }
                }
            }
        }
    }

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
        "embeddings_failed",
        &last_error.unwrap_or_else(|| {
            format!(
                "No available providers with embeddings support for model '{}'",
                model
            )
        }),
    )
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
