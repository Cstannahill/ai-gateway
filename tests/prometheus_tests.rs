//! `GET /metrics/prometheus`, end to end: real requests through the gateway to a
//! mock provider that reports `usage.cost` (as OpenRouter does), then a scrape.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};
use ai_gateway::types::ProviderConfig;

async fn upstream(body: Bytes) -> axum::response::Response {
    let stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    if stream {
        // OpenRouter sends usage (with cost) in the last chunk before [DONE].
        let sse = concat!(
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4,\"cost\":0.005}}\n\n",
            "data: [DONE]\n\n",
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    axum::Json(serde_json::json!({
        "id": "c", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2, "cost": 0.0075}
    }))
    .into_response()
}

fn gateway(base_url: &str, extra_config: impl FnOnce(&mut GatewayConfig)) -> Router {
    let provider: ProviderConfig = serde_yaml::from_str(&format!(
        "name: p\ntype: open_ai_compatible\nbase_url: \"{base_url}\"\ntimeout_secs: 10\n"
    ))
    .unwrap();
    let adapter = providers::build_adapter(&provider).unwrap();
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(provider, adapter);
    let mut config = GatewayConfig::load(None).unwrap();
    extra_config(&mut config);
    let (telemetry_tx, rx) = tokio::sync::mpsc::channel(1000);
    std::mem::forget(rx);
    build_router(Arc::new(AppState {
        config: config.into(),
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
        reloader: None,
    }))
}

async fn call(app: &Router, method: &str, path: &str, body: Option<serde_json::Value>, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(path).header("content-type", "application/json");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

async fn chat(app: &Router, stream: bool) {
    let (status, _, text) = call(
        app,
        "POST",
        "/v1/chat/completions",
        Some(serde_json::json!({"model": "p/m", "stream": stream, "messages": [{"role": "user", "content": "hi"}]})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

fn metric(text: &str, name: &str, labels: &str) -> Option<f64> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (head, value) = l.rsplit_once(' ')?;
            let (n, rest) = head.split_once('{').map_or((head, ""), |(n, r)| (n, r.trim_end_matches('}')));
            (n == name && rest == labels).then(|| value.parse().ok()).flatten()
        })
}

async fn serve() -> String {
    let app = Router::new().fallback(|body: Bytes| upstream(body));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_scrape_reflects_traffic_and_provider_reported_cost() {
    let app = gateway(&serve().await, |_| {});

    // One non-streaming and one streaming request; the provider reports
    // 0.0075 and 0.005 USD respectively.
    chat(&app, false).await;
    chat(&app, true).await;

    let (status, headers, text) = call(&app, "GET", "/metrics/prometheus", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/plain; version=0.0.4; charset=utf-8");

    assert_eq!(metric(&text, "gateway_requests_total", ""), Some(2.0), "{text}");
    assert_eq!(metric(&text, "gateway_provider_requests_total", "provider=\"p\""), Some(2.0));
    assert_eq!(metric(&text, "gateway_provider_errors_total", "provider=\"p\""), Some(0.0));
    assert_eq!(metric(&text, "gateway_provider_active_requests", "provider=\"p\""), Some(0.0));
    let cost = metric(&text, "gateway_provider_reported_cost_usd_total", "provider=\"p\"").unwrap();
    assert!((cost - 0.0125).abs() < 1e-9, "both the streamed and the non-streamed cost count: {cost}");
    assert_eq!(
        metric(&text, "gateway_provider_request_duration_seconds_count", "provider=\"p\""),
        Some(2.0)
    );
    assert_eq!(
        metric(&text, "gateway_provider_request_duration_seconds_bucket", "provider=\"p\",le=\"+Inf\""),
        Some(2.0)
    );
    assert_eq!(metric(&text, "gateway_provider_circuit_open", "provider=\"p\""), Some(0.0));
    assert!(metric(&text, "gateway_uptime_seconds", "").is_some());
    // Caching is off, so its families must be absent rather than zero.
    assert!(!text.contains("gateway_cache_"));
}

#[tokio::test]
async fn the_json_metrics_endpoint_is_unchanged() {
    let app = gateway(&serve().await, |_| {});
    chat(&app, false).await;
    let (status, headers, body) = call(&app, "GET", "/metrics", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"].to_str().unwrap().starts_with("application/json"));
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["gateway"]["total_requests"], 1);
    assert_eq!(json["providers"][0]["name"], "p");
}

#[tokio::test]
async fn the_scrape_endpoint_refuses_browsers_and_foreign_hosts() {
    let app = gateway(&serve().await, |_| {});
    for headers in [
        vec![("origin", "https://evil.example")],
        vec![("sec-fetch-site", "cross-site")],
        vec![("host", "evil.example")],
    ] {
        let (status, _, _) = call(&app, "GET", "/metrics/prometheus", None, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
    }
    // What a Prometheus scraper sends: a loopback Host, no Origin.
    let (status, _, _) = call(&app, "GET", "/metrics/prometheus", None, &[("host", "127.0.0.1:8999")]).await;
    assert_eq!(status, StatusCode::OK);
}
