use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use url::Url;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers::openai_compatible::OpenAICompatibleAdapter;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{build_router, AppState};
use ai_gateway::types::request::{GatewayRequest, MessageContent, Role};
use ai_gateway::types::{ModelCapabilities, ProviderConfig};

#[test]
fn test_nullable_assistant_message_content() {
    // 1. Assistant message with content: null and tool_calls
    let json_with_null = r#"{
        "model": "gpt-4o",
        "messages": [
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\": \"Tokyo\"}"
                        }
                    }
                ]
            }
        ]
    }"#;

    let req: Result<GatewayRequest, _> = serde_json::from_str(json_with_null);
    assert!(req.is_ok(), "Failed to deserialize message with content: null: {:?}", req.err());
    let req = req.unwrap();
    let messages = req.messages.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, Role::Assistant);
    assert!(messages[0].content.is_none());
    assert!(messages[0].tool_calls.is_some());

    // 2. Standard text content
    let json_with_text = r#"{
        "model": "gpt-4o",
        "messages": [
            {
                "role": "user",
                "content": "Hello world"
            }
        ]
    }"#;
    let req: GatewayRequest = serde_json::from_str(json_with_text).unwrap();
    let messages = req.messages.unwrap();
    assert_eq!(messages[0].content, Some(MessageContent::Text("Hello world".to_string())));

    // 3. Message with omitted content field
    let json_with_missing_content = r#"{
        "model": "gpt-4o",
        "messages": [
            {
                "role": "assistant"
            }
        ]
    }"#;
    let req: GatewayRequest = serde_json::from_str(json_with_missing_content).unwrap();
    let messages = req.messages.unwrap();
    assert!(messages[0].content.is_none());
}

#[test]
fn test_endpoint_normalization() {
    let make_adapter = |base_url: &str| {
        let config = ProviderConfig {
            name: "test-provider".to_string(),
            r#type: ai_gateway::types::ProviderType::OpenAiCompatible,
            base_url: Url::parse(base_url).unwrap(),
            api_key: None,
            api_key_env: None,
            default_headers: None,
            timeout_secs: 30,
            max_concurrent: None,
                rate_limit_per_sec: None,
                circuit_breaker_threshold: None,
                circuit_breaker_cooldown_secs: 30,
            enabled: true,
        };
        OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap()
    };

    // Case 1: base_url ends with /v1, path starts with /v1
    let adapter1 = make_adapter("https://api.openai.com/v1");
    assert_eq!(
        adapter1.endpoint("/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        adapter1.endpoint("/v1/models"),
        "https://api.openai.com/v1/models"
    );

    // Case 2: base_url ends with /v1/, path starts with /v1
    let adapter2 = make_adapter("https://api.openai.com/v1/");
    assert_eq!(
        adapter2.endpoint("/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );

    // Case 3: base_url does not end with /v1, path starts with /v1
    let adapter3 = make_adapter("http://localhost:11434");
    assert_eq!(
        adapter3.endpoint("/v1/chat/completions"),
        "http://localhost:11434/v1/chat/completions"
    );

    // Case 4: base_url has nested path ending in /v1
    let adapter4 = make_adapter("https://openrouter.ai/api/v1");
    assert_eq!(
        adapter4.endpoint("/v1/chat/completions"),
        "https://openrouter.ai/api/v1/chat/completions"
    );
}

#[test]
fn test_provider_registry_default() {
    let registry = ProviderRegistry::default();
    assert_eq!(registry.provider_names().len(), 0);
}

#[test]
fn test_multibyte_utf8_split_buffer_safety() {
    // Simulate multi-byte UTF-8 split across TCP byte chunks.
    // Rocket emoji 🚀 is 4 bytes: [0xF0, 0x9F, 0x9A, 0x80]
    let chunk1: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"\xF0\x9F";
    let chunk2: &[u8] = b"\x9A\x80\"}}]}\n\n";

    let mut buffer: Vec<u8> = Vec::new();
    let mut decoded_lines: Vec<String> = Vec::new();

    for chunk in [chunk1, chunk2] {
        buffer.extend_from_slice(chunk);
        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line_bytes = buffer.drain(..=pos).collect::<Vec<u8>>();
            let line_str = std::str::from_utf8(&line_bytes).unwrap_or("");
            let trimmed = line_str.trim();
            if !trimmed.is_empty() {
                decoded_lines.push(trimmed.to_string());
            }
        }
    }

    assert_eq!(decoded_lines.len(), 1);
    assert!(decoded_lines[0].contains("🚀"));
    assert!(!decoded_lines[0].contains('\u{FFFD}'), "Corrupted UTF-8 replacement character found!");
}

#[tokio::test]
async fn test_models_endpoint_namespacing() {
    // Set up app state with a provider
    let config = GatewayConfig::load(None).unwrap();
    let model_registry = ModelRegistry::new(HashMap::new());
    let provider_registry = ProviderRegistry::new();
    let router = RoutingEngine::new("priority".to_string());
    let (telemetry_tx, _rx) = tokio::sync::mpsc::channel(100);

    let state = Arc::new(AppState {
        config,
        model_registry,
        provider_registry,
        router,
        telemetry_tx,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        analytics_pool: None,
    });

    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());
}

#[test]
fn test_non_streaming_tool_calls_and_reasoning_extraction() {
    let raw_response = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1677652288,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Calling function",
                "reasoning_content": "User asked for weather in Tokyo",
                "tool_calls": [{
                    "id": "call_tokyo_123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Tokyo\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 15,
            "completion_tokens": 30,
            "total_tokens": 45
        }
    });

    let message = &raw_response["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default().to_string();
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    assert_eq!(content, "Calling function");
    assert_eq!(reasoning, "User asked for weather in Tokyo");

    let tool_calls = message["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_tokyo_123");
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    assert_eq!(tool_calls[0]["function"]["arguments"], "{\"city\":\"Tokyo\"}");
}

#[test]
fn test_multi_choice_non_streaming_response_yields_all_choices() {
    // `n > 1` previously only ever surfaced `choices[0]`; the others were
    // silently dropped (Audit A08 / docs/REMEDIATION_PLAN.md Phase 2.3).
    use ai_gateway::providers::events::ProviderEvent;

    let response_body = serde_json::json!({
        "id": "chatcmpl-multi",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": "first answer" },
                "finish_reason": "stop"
            },
            {
                "index": 1,
                "message": { "role": "assistant", "content": "second answer" },
                "finish_reason": "stop"
            }
        ],
        "usage": { "prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18 }
    });

    let events = OpenAICompatibleAdapter::events_from_non_streaming_response(
        &response_body,
        chrono::Utc::now(),
        "test-provider".to_string(),
        "test-model".to_string(),
    );

    let content_deltas: Vec<(u32, String)> = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::ContentDelta { index, delta, .. } => Some((*index, delta.clone())),
            _ => None,
        })
        .collect();

    assert_eq!(
        content_deltas,
        vec![(0, "first answer".to_string()), (1, "second answer".to_string())]
    );
}

#[test]
fn test_stop_accepts_string_or_array() {
    // OpenAI's Chat Completions API allows `stop` to be a single string or
    // an array of strings; a bare string previously failed to deserialize
    // at all (Audit A08).
    let single: GatewayRequest =
        serde_json::from_value(serde_json::json!({"model": "m", "stop": "\n\n"})).unwrap();
    assert_eq!(single.stop, Some(vec!["\n\n".to_string()]));

    let many: GatewayRequest =
        serde_json::from_value(serde_json::json!({"model": "m", "stop": ["a", "b"]})).unwrap();
    assert_eq!(many.stop, Some(vec!["a".to_string(), "b".to_string()]));

    let absent: GatewayRequest =
        serde_json::from_value(serde_json::json!({"model": "m"})).unwrap();
    assert_eq!(absent.stop, None);

    let null: GatewayRequest =
        serde_json::from_value(serde_json::json!({"model": "m", "stop": null})).unwrap();
    assert_eq!(null.stop, None);
}
