//! Client-compatibility behaviour, checked end to end (HTTP in -> gateway ->
//! mock upstream over real TCP):
//!
//! * unknown top-level request fields reach the provider (and `drop_params`
//!   strips them again),
//! * `POST /v1/completions` is forwarded to the provider's own
//!   `/v1/completions` (raw prompt, `suffix`), streaming and not,
//! * `Retry-After` is honoured (short: wait and retry; long: fall back and
//!   cool the provider down instead of hammering it),
//! * every successful response says which provider/model served it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};
use ai_gateway::types::ProviderConfig;

// ─── Mock upstream ───────────────────────────────────────────────────────────

/// A scripted reply: status, extra headers, body.
type Reply = (u16, Vec<(&'static str, &'static str)>, String);

#[derive(Default)]
struct Upstream {
    /// (path, parsed JSON body) for every request received, in order.
    hits: Mutex<Vec<(String, serde_json::Value)>>,
    /// Replies consumed front-to-back before falling back to the default 200.
    script: Mutex<VecDeque<Reply>>,
}

async fn upstream_handler(State(up): State<Arc<Upstream>>, _m: Method, uri: Uri, _h: HeaderMap, body: Bytes) -> Response {
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let path = uri.path().to_string();
    up.hits.lock().unwrap().push((path.clone(), json.clone()));

    if let Some((status, headers, body)) = up.script.lock().unwrap().pop_front() {
        let mut response = (StatusCode::from_u16(status).unwrap(), body).into_response();
        for (name, value) in headers {
            response.headers_mut().insert(name, value.parse().unwrap());
        }
        return response;
    }

    let stream = json.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    let completions = path.ends_with("/completions") && !path.ends_with("/chat/completions");
    if stream {
        let sse = if completions {
            concat!(
                "data: {\"id\":\"c1\",\"object\":\"text_completion\",\"choices\":[{\"index\":0,\"text\":\"hel\",\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"text_completion\",\"choices\":[{\"index\":0,\"text\":\"lo\",\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"text_completion\",\"choices\":[{\"index\":0,\"text\":\"\",\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
                "data: [DONE]\n\n",
            )
        } else {
            concat!(
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            )
        };
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    if completions {
        return axum::Json(serde_json::json!({
            "id": "c1", "object": "text_completion", "created": 0, "model": "m",
            "choices": [{"index": 0, "text": " completed", "logprobs": null, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
        }))
        .into_response();
    }
    axum::Json(serde_json::json!({
        "id": "c1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    }))
    .into_response()
}

async fn start_upstream() -> (String, Arc<Upstream>) {
    let up = Arc::new(Upstream::default());
    let app = Router::new().fallback(upstream_handler).with_state(up.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), up)
}

// ─── Gateway under test ──────────────────────────────────────────────────────

fn gateway(base_url: &str, provider_extra: &str, tune: impl FnOnce(&mut GatewayConfig)) -> Router {
    let provider: ProviderConfig = serde_yaml::from_str(&format!(
        "name: mock\ntype: open_ai_compatible\nbase_url: \"{base_url}\"\ntimeout_secs: 10\n{provider_extra}"
    ))
    .unwrap();
    let adapter = providers::build_adapter(&provider).unwrap();
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(provider, adapter);
    // Health starts as `Unknown`, which routing treats as available.

    let mut config = GatewayConfig::load(None).unwrap();
    tune(&mut config);

    let (telemetry_tx, rx) = tokio::sync::mpsc::channel(1000);
    std::mem::forget(rx); // keep the channel open for the test's lifetime
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry,
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
    });
    build_router(state)
}

async fn post(app: &Router, path: &str, body: serde_json::Value) -> (StatusCode, HeaderMap, Bytes) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes)
}

fn chat_body(extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": "mock/m",
        "messages": [{"role": "user", "content": "ping"}],
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    body
}

fn last_hit(up: &Upstream) -> (String, serde_json::Value) {
    up.hits.lock().unwrap().last().cloned().expect("upstream received no request")
}

// ─── Unknown fields ──────────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_top_level_fields_are_forwarded_to_the_provider() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    let (status, _, _) = post(
        &app,
        "/v1/chat/completions",
        chat_body(serde_json::json!({
            "logprobs": true,
            "top_logprobs": 3,
            "top_k": 40,
            "logit_bias": {"50256": -100},
            "stream_options": {"include_usage": true},
            "temperature": 0.25,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (path, body) = last_hit(&up);
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(body["logprobs"], true);
    assert_eq!(body["top_logprobs"], 3);
    assert_eq!(body["top_k"], 40);
    assert_eq!(body["logit_bias"]["50256"], -100);
    assert_eq!(body["stream_options"]["include_usage"], true);
    // Typed fields are untouched by the pass-through.
    assert_eq!(body["temperature"], 0.25);
    assert_eq!(body["model"], "m");
}

#[tokio::test]
async fn provider_options_override_a_forwarded_unknown_field() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    post(
        &app,
        "/v1/chat/completions",
        chat_body(serde_json::json!({"top_k": 40, "provider_options": {"mock": {"top_k": 5}}})),
    )
    .await;
    assert_eq!(last_hit(&up).1["top_k"], 5);
}

#[tokio::test]
async fn drop_params_strips_fields_before_the_request_leaves_the_gateway() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "drop_params: [logprobs, parallel_tool_calls]\n", |_| {});

    post(
        &app,
        "/v1/chat/completions",
        chat_body(serde_json::json!({"logprobs": true, "top_k": 7, "parallel_tool_calls": false})),
    )
    .await;
    let body = last_hit(&up).1;
    assert!(body.get("logprobs").is_none(), "dropped: {body}");
    assert!(body.get("parallel_tool_calls").is_none(), "typed fields can be dropped too: {body}");
    assert_eq!(body["top_k"], 7, "fields not on the list still go through");
}

// ─── /v1/completions ─────────────────────────────────────────────────────────

#[tokio::test]
async fn completions_are_forwarded_to_the_providers_completions_endpoint() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    let (status, headers, bytes) = post(
        &app,
        "/v1/completions",
        serde_json::json!({
            "model": "mock/m",
            "prompt": "def add(a, b):",
            "suffix": "\nprint(add(1, 2))",
            "max_tokens": 16,
            "temperature": 0,
            "top_k": 20,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let (path, body) = last_hit(&up);
    assert_eq!(path, "/v1/completions", "must not be wrapped into a chat call");
    assert_eq!(body["prompt"], "def add(a, b):");
    assert_eq!(body["suffix"], "\nprint(add(1, 2))");
    assert_eq!(body["max_tokens"], 16);
    assert_eq!(body["top_k"], 20);
    assert!(body.get("messages").is_none(), "{body}");

    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["object"], "text_completion");
    assert!(json["id"].as_str().unwrap().starts_with("cmpl-"), "{json}");
    assert_eq!(json["choices"][0]["text"], " completed");
    assert_eq!(json["choices"][0]["finish_reason"], "length");
    assert_eq!(json["usage"]["total_tokens"], 5);
    assert_eq!(headers["x-gateway-provider"], "mock");
}

#[tokio::test]
async fn streaming_completions_emit_text_completion_chunks() {
    let (base, _up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    let (status, _, bytes) = post(
        &app,
        "/v1/completions",
        serde_json::json!({"model": "mock/m", "prompt": "hi", "stream": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let chunks: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    assert!(text.contains("[DONE]"));
    assert!(chunks.iter().all(|c| c["object"] == "text_completion"), "{chunks:?}");
    let streamed: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["text"].as_str())
        .collect();
    assert_eq!(streamed, "hello");
    assert!(
        chunks.iter().any(|c| c["choices"][0]["finish_reason"] == "stop"),
        "a final chunk must carry the finish reason: {chunks:?}"
    );
    assert!(
        chunks.iter().any(|c| c["usage"]["total_tokens"] == 5),
        "usage must be reported: {chunks:?}"
    );
}

#[tokio::test]
async fn completions_without_a_prompt_are_rejected_before_any_provider_call() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    let (status, _, bytes) = post(&app, "/v1/completions", serde_json::json!({"model": "mock/m"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&bytes).contains("prompt"));
    assert!(up.hits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stray_prompt_field_cannot_turn_a_chat_request_into_a_completion() {
    let (base, up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    let (status, _, _) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({"prompt": "x", "suffix": "y"}))).await;
    assert_eq!(status, StatusCode::OK);
    let (path, body) = last_hit(&up);
    assert_eq!(path, "/v1/chat/completions");
    assert!(body.get("prompt").is_none() && body.get("suffix").is_none(), "{body}");
}

// ─── Response headers ────────────────────────────────────────────────────────

#[tokio::test]
async fn responses_name_the_provider_model_and_attempts_that_served_them() {
    let (base, _up) = start_upstream().await;
    let app = gateway(&base, "", |_| {});

    for stream in [false, true] {
        let (status, headers, _) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({"stream": stream}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-gateway-provider"], "mock", "stream={stream}");
        assert_eq!(headers["x-gateway-model"], "m", "stream={stream}");
        assert_eq!(headers["x-gateway-attempts"], "1", "stream={stream}");
        let id = headers["x-gateway-request-id"].to_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{id}");
    }
}

// ─── Retry-After ─────────────────────────────────────────────────────────────

fn rate_limited(retry_after: &'static str) -> Reply {
    (
        429,
        vec![("retry-after", retry_after)],
        r#"{"error":{"message":"slow down","type":"rate_limit"}}"#.to_string(),
    )
}

#[tokio::test]
async fn a_short_retry_after_is_waited_out_and_the_request_succeeds() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(rate_limited("1"));
    let app = gateway(&base, "", |c| {
        c.routing.max_retries_per_provider = 2;
        c.routing.max_retry_after_secs = 5;
    });

    let started = Instant::now();
    let (status, headers, _) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({}))).await;
    let waited = started.elapsed();

    assert_eq!(status, StatusCode::OK);
    assert!(waited >= Duration::from_millis(990), "must honour Retry-After: 1, waited {waited:?}");
    assert!(waited < Duration::from_secs(4), "waited far longer than asked: {waited:?}");
    assert_eq!(up.hits.lock().unwrap().len(), 2, "one 429 + one retry");
    assert_eq!(headers["x-gateway-attempts"], "2");
}

#[tokio::test]
async fn a_long_retry_after_is_not_waited_out_and_cools_the_provider_down() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(rate_limited("120"));
    let app = gateway(&base, "", |c| {
        c.routing.max_retries_per_provider = 3;
        c.routing.max_retry_after_secs = 5;
    });

    let started = Instant::now();
    let (status, _, bytes) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({}))).await;
    assert!(started.elapsed() < Duration::from_secs(3), "must not hold the client for 120s");
    assert_ne!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&bytes).contains("slow down"), "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(up.hits.lock().unwrap().len(), 1, "no in-place retries against a provider that said wait 120s");

    // The provider is now skipped by routing: the next request never reaches it.
    let (status, _, bytes) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", String::from_utf8_lossy(&bytes));
    assert!(String::from_utf8_lossy(&bytes).contains("rejected"), "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(up.hits.lock().unwrap().len(), 1, "cooled-down provider must not be called");
}

#[tokio::test]
async fn a_non_json_error_body_still_yields_a_clean_status_and_message() {
    let (base, up) = start_upstream().await;
    for _ in 0..4 {
        up.script
            .lock()
            .unwrap()
            .push_back((503, vec![], "<html>upstream is down</html>".to_string()));
    }
    let app = gateway(&base, "", |c| c.routing.max_retries_per_provider = 1);

    let (status, _, bytes) = post(&app, "/v1/chat/completions", chat_body(serde_json::json!({}))).await;
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{text}");
    assert!(text.contains("upstream is down"), "the provider's own message should survive: {text}");
}

// ─── Gemini has no /v1/completions ───────────────────────────────────────────

#[tokio::test]
async fn gemini_refuses_completions_with_a_fallback_worthy_error() {
    use ai_gateway::providers::events::ProviderEvent;
    use ai_gateway::providers::ProviderOutput;
    use ai_gateway::routing::fallback::{RetryConfig, should_fallback};
    use ai_gateway::types::GatewayRequest;

    let config: ProviderConfig = serde_yaml::from_str(
        "name: gemini\ntype: gemini\nbase_url: \"http://127.0.0.1:9/v1beta/openai\"\ntimeout_secs: 1\n",
    )
    .unwrap();
    let adapter = providers::build_adapter(&config).unwrap();
    let request = GatewayRequest {
        model: "gemini/x".into(),
        prompt: Some(serde_json::json!("hello")),
        ..Default::default()
    };
    // Refused locally: no network involved (the port above is closed).
    let ProviderOutput::Complete(ProviderEvent::Error { code, retryable, status_code, .. }) =
        adapter.execute(&request, "x").await.unwrap()
    else {
        panic!("expected an immediate error");
    };
    assert_eq!(code, "unsupported_endpoint");
    assert!(!retryable);
    assert_eq!(status_code, Some(400));
    // Even with an empty `fallback_on`, routing moves on to the next candidate.
    let strict = RetryConfig { fallback_on: vec![], ..Default::default() };
    assert!(should_fallback(&strict, &code, status_code));
}
