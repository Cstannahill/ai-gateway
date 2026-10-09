use chrono::Utc;
use ai_gateway::providers::events::ProviderEvent;
use ai_gateway::server::routes::chat_completions::event_to_sse;

#[test]
fn test_sse_content_delta_envelope() {
    let event = ProviderEvent::ContentDelta {
        timestamp: Utc::now(),
        index: 0,
        delta: "Hello, world!".to_string(),
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-123", 1700000000, "test-model").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["id"], "chatcmpl-test-123");
    assert_eq!(json["object"], "chat.completion.chunk");
    assert_eq!(json["created"], 1700000000);
    assert_eq!(json["model"], "test-model");
    assert_eq!(json["choices"][0]["index"], 0);
    assert_eq!(json["choices"][0]["delta"]["content"], "Hello, world!");
    assert!(json["choices"][0]["finish_reason"].is_null());
}

#[test]
fn test_sse_reasoning_delta_envelope() {
    let event = ProviderEvent::ReasoningDelta {
        timestamp: Utc::now(),
        delta: "Thinking through the solution...".to_string(),
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-456", 1700000000, "deepseek-r1").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["id"], "chatcmpl-test-456");
    assert_eq!(json["choices"][0]["delta"]["reasoning_content"], "Thinking through the solution...");
}

#[test]
fn test_sse_tool_call_delta_envelope() {
    let event = ProviderEvent::ToolCallDelta {
        timestamp: Utc::now(),
        index: 0,
        tool_call_index: 0,
        id: Some("call_abc123".to_string()),
        name: Some("search_web".to_string()),
        arguments: Some(r#"{"query":"# .to_string()),
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-789", 1700000000, "gpt-4o").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["choices"][0]["delta"]["tool_calls"][0]["id"], "call_abc123");
    assert_eq!(json["choices"][0]["delta"]["tool_calls"][0]["type"], "function");
    assert_eq!(json["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "search_web");
    assert_eq!(json["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], r#"{"query":"#);
}

#[test]
fn test_sse_completed_envelope() {
    let event = ProviderEvent::Completed {
        timestamp: Utc::now(),
        finish_reason: Some("stop".to_string()),
        provider_response_id: Some("prov-resp-1".to_string()),
        upstream_provider: None,
        raw_response: None,
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-end", 1700000000, "test-model").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["choices"][0]["finish_reason"], "stop");
    assert!(json["choices"][0]["delta"].as_object().unwrap().is_empty());
}

#[test]
fn test_sse_error_envelope() {
    // Previously fell into the catch-all `_ => None` and vanished from the
    // client entirely (Audit A04).
    let event = ProviderEvent::Error {
        timestamp: Utc::now(),
        code: "server_error".to_string(),
        message: "upstream exploded".to_string(),
        status_code: Some(503),
        retryable: true,
        retry_after_ms: None,
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-err", 1700000000, "test-model").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["error"]["message"], "upstream exploded");
    assert_eq!(json["error"]["code"], "server_error");
}

#[test]
fn test_sse_usage_update_envelope() {
    // Previously discarded entirely (Audit A08) despite usage being one of
    // the explicit metrics the gateway exists to capture.
    let event = ProviderEvent::UsageUpdate {
        timestamp: Utc::now(),
        prompt_tokens: Some(10),
        completion_tokens: Some(20),
        total_tokens: Some(30),
        cached_tokens: Some(5),
        reasoning_tokens: Some(2),
        raw_usage: None,
    };

    let sse_str = event_to_sse(&event, "chatcmpl-test-usage", 1700000000, "test-model").expect("Should format SSE");
    let json: serde_json::Value = serde_json::from_str(&sse_str).expect("Should parse as valid JSON");

    assert_eq!(json["usage"]["prompt_tokens"], 10);
    assert_eq!(json["usage"]["completion_tokens"], 20);
    assert_eq!(json["usage"]["total_tokens"], 30);
    assert_eq!(json["usage"]["prompt_tokens_details"]["cached_tokens"], 5);
    assert_eq!(json["usage"]["completion_tokens_details"]["reasoning_tokens"], 2);
}

#[test]
fn test_sse_lifecycle_events_filtered_out() {
    let started = ProviderEvent::Started {
        timestamp: Utc::now(),
        provider: "ollama".to_string(),
        model: "llama3".to_string(),
    };
    assert!(event_to_sse(&started, "id", 0, "m").is_none());

    let connected = ProviderEvent::Connected {
        timestamp: Utc::now(),
        connection_latency_us: 1500,
    };
    assert!(event_to_sse(&connected, "id", 0, "m").is_none());
}
