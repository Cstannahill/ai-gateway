use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use uuid::Uuid;

use crate::server::AppState;
use crate::telemetry::events::TelemetryEvent;

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
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::now_v7);
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

    let mut last_error = None;
    let mut attempt_number = 0u32;

    for candidate in &resolved.candidates {
        attempt_number += 1;
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

        state.provider_registry.record_request_start(&candidate.provider);

        match provider.embeddings(&candidate.model, &input).await {
            Ok(result) => {
                let latency_ms = (Utc::now() - received_at).num_milliseconds().max(0) as u64;
                state.provider_registry.record_request_end(&candidate.provider, false);
                state.provider_registry.record_latency(&candidate.provider, latency_ms);

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

                return Json(result).into_response();
            }
            Err(e) => {
                state.provider_registry.record_request_end(&candidate.provider, true);
                last_error = Some(e.to_string());
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
