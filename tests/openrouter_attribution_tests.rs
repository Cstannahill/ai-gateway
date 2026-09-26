//! Wire-level check that every HTTP request the OpenRouter adapter sends
//! carries OpenRouter's app-attribution headers (models, health, non-streaming
//! chat, streaming chat). A local mock server records what actually arrives, so
//! this catches a code path that builds its own request and forgets the headers.
//!
//! Header rules: https://openrouter.ai/docs/app-attribution

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;

use ai_gateway::providers::openrouter::{DEFAULT_REFERER, DEFAULT_TITLE};
use ai_gateway::providers::{self, ProviderOutput};
use ai_gateway::types::{GatewayRequest, Message, MessageContent, ProviderConfig, Role};

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(String, String, HeaderMap)>>>);

async fn mock(State(rec): State<Recorder>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    rec.0.lock().unwrap().push((method.to_string(), uri.path().to_string(), headers));
    let wants_stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);

    if uri.path().ends_with("/models") {
        return axum::Json(serde_json::json!({"object": "list", "data": [{"id": "openai/gpt-4o-mini"}]})).into_response();
    }
    if wants_stream {
        let sse = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    axum::Json(serde_json::json!({
        "id": "c1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    }))
    .into_response()
}

async fn start_mock() -> (String, Recorder) {
    let rec = Recorder::default();
    let app = Router::new().fallback(mock).with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), rec)
}

fn provider_config(base_url: &str, extra: &str) -> ProviderConfig {
    serde_yaml::from_str(&format!(
        "name: openrouter\ntype: open_router\nbase_url: \"{base_url}\"\ntimeout_secs: 10\n{extra}"
    ))
    .unwrap()
}

fn chat(stream: bool) -> GatewayRequest {
    GatewayRequest {
        model: "openai/gpt-4o-mini".into(),
        messages: Some(vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("ping".into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }]),
        stream,
        ..Default::default()
    }
}

/// Drive every outbound call the adapter can make.
async fn exercise(config: &ProviderConfig) {
    let adapter = providers::build_adapter(config).unwrap();
    adapter.models().await.unwrap();
    adapter.health().await;
    for stream in [false, true] {
        match adapter.execute(&chat(stream), "openai/gpt-4o-mini").await.unwrap() {
            ProviderOutput::Stream(mut s) => while s.next().await.is_some() {},
            ProviderOutput::Complete(_) => {}
        }
    }
}

fn value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn every_outbound_request_carries_attribution_headers() {
    let (base, rec) = start_mock().await;
    exercise(&provider_config(&base, "")).await;

    let seen = rec.0.lock().unwrap();
    let paths: Vec<String> = seen.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
    // models(), health(), chat non-streaming, chat streaming: nothing skipped.
    assert!(seen.len() >= 4, "expected at least 4 requests, saw {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("/models")), "{paths:?}");
    assert_eq!(paths.iter().filter(|p| p.starts_with("POST") && p.ends_with("/chat/completions")).count(), 2, "{paths:?}");

    for (method, path, headers) in seen.iter() {
        assert_eq!(value(headers, "http-referer"), Some(DEFAULT_REFERER), "{method} {path}");
        assert_eq!(value(headers, "x-openrouter-title"), Some(DEFAULT_TITLE), "{method} {path}");
        assert_eq!(headers.get_all("http-referer").iter().count(), 1, "{method} {path}: duplicate referer");
        assert!(headers.get("x-title").is_none(), "{method} {path}: legacy title must not be sent by default");
    }
}

#[tokio::test]
async fn operator_overrides_reach_the_wire_and_replace_defaults() {
    let (base, rec) = start_mock().await;
    let extra = "default_headers:\n  http-referer: \"https://me.example\"\n  X-Title: \"My App\"\n  X-OpenRouter-Categories: \"cli-agent\"\n  X-OpenRouter-App-Visibility: \"hidden\"\n";
    exercise(&provider_config(&base, extra)).await;

    let seen = rec.0.lock().unwrap();
    assert!(!seen.is_empty());
    for (method, path, headers) in seen.iter() {
        let ctx = format!("{method} {path}");
        assert_eq!(value(headers, "http-referer"), Some("https://me.example"), "{ctx}");
        assert_eq!(headers.get_all("http-referer").iter().count(), 1, "{ctx}");
        assert_eq!(value(headers, "x-title"), Some("My App"), "{ctx}");
        assert!(headers.get("x-openrouter-title").is_none(), "{ctx}: default title must not be added next to an operator title");
        assert_eq!(value(headers, "x-openrouter-categories"), Some("cli-agent"), "{ctx}");
        assert_eq!(value(headers, "x-openrouter-app-visibility"), Some("hidden"), "{ctx}");
    }
}
