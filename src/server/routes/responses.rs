use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use futures::StreamExt;
use uuid::Uuid;

use crate::providers::events::ProviderEvent;
use crate::providers::ProviderOutput;
use crate::server::AppState;
use crate::telemetry::events::TelemetryEvent;
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
                }
            }
        }
    }

    let request = GatewayRequest {
        request_id,
        received_at,
        model: model.clone(),
        messages: Some(messages),
        input: body.get("input").cloned(),
        temperature: body.get("temperature").and_then(|t| t.as_f64()),
        top_p: body.get("top_p").and_then(|t| t.as_f64()),
        max_tokens: body.get("max_tokens").or_else(|| body.get("max_output_tokens")).and_then(|m| m.as_u64()),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        response_format: None,
        reasoning: None,
        modalities: None,
        n: None,
        frequency_penalty: None,
        presence_penalty: None,
        seed: None,
        user: body.get("user").and_then(|u| u.as_str()).map(|s| s.to_string()),
        provider_options: None,
        routing: None,
        metadata: None,
    };

    // Emit telemetry
    state.emit_telemetry(TelemetryEvent::RequestReceived {
        request_id,
        timestamp: received_at,
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

    let mut attempt_number = 0u32;
    let mut last_error = None;

    for candidate in &routing_plan.candidates {
        attempt_number += 1;
        let provider = match state.provider_registry.get(&candidate.provider) {
            Some(p) => p,
            None => continue,
        };

        state.provider_registry.record_request_start(&candidate.provider);

        match provider.execute(&request, &candidate.model).await {
            Ok(ProviderOutput::Stream(stream)) => {
                state.provider_registry.record_request_end(&candidate.provider, false);

                let instrumented = InstrumentedStream::new(
                    stream,
                    request_id,
                    attempt_number,
                    state.telemetry_tx.clone(),
                    state.config.telemetry.store_stream_events,
                );

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
                            usage = Some(serde_json::json!({
                                "prompt_tokens": prompt_tokens.unwrap_or(0),
                                "completion_tokens": completion_tokens.unwrap_or(0),
                                "total_tokens": total_tokens.unwrap_or(0),
                            }));
                        }
                        ProviderEvent::Error { message, .. } => {
                            stream_err = Some(message);
                            break;
                        }
                        _ => {}
                    }
                }

                if let Some(msg) = stream_err {
                    state.provider_registry.record_request_end(&candidate.provider, true);
                    last_error = Some(msg);
                    continue;
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
            Ok(ProviderOutput::Complete(ProviderEvent::Error { message, .. })) => {
                state.provider_registry.record_request_end(&candidate.provider, true);
                last_error = Some(message);
            }
            Ok(_) => {
                state.provider_registry.record_request_end(&candidate.provider, false);
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
