//! `GET /v1/models` must satisfy the read contract the dashboard validates:
//! every model - configured alias or provider-discovered - has a
//! `capabilities` OBJECT with boolean flags, never `null`. A single `null`
//! makes the frontend reject the whole list ("0 discovered models").

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use url::Url;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers::Provider;
use ai_gateway::providers::openai_compatible::OpenAICompatibleAdapter;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};
use ai_gateway::types::{ModelCapabilities, ProviderConfig, ProviderType};

/// A provider whose /v1/models lists ids only (like Ollama, OpenRouter, ...).
async fn start_provider_mock() -> String {
    let app = Router::new().fallback(|| async {
        axum::Json(serde_json::json!({
            "object": "list",
            "data": [{"id": "llama3:8b", "object": "model"}, {"id": "qwen3:4b"}]
        }))
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn discovered_models_have_a_capabilities_object_not_null() {
    let base_url = start_provider_mock().await;
    let config = ProviderConfig {
        name: "mock".to_string(),
        r#type: ProviderType::OpenAiCompatible,
        base_url: Url::parse(&base_url).unwrap(),
        api_key: None,
        api_key_env: None,
        default_headers: None,
        timeout_secs: 5,
        max_concurrent: None,
        rate_limit_per_sec: None,
        degraded_error_rate_threshold: None,
        circuit_breaker_threshold: None,
        circuit_breaker_cooldown_secs: 30,
        drop_params: Vec::new(),
        enabled: true,
    };
    let adapter: Arc<dyn Provider> =
        Arc::new(OpenAICompatibleAdapter::new(&config, ModelCapabilities { streaming: true, ..Default::default() }).unwrap());
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(config, adapter);

    let (telemetry_tx, _rx) = tokio::sync::mpsc::channel(10);
    let state = Arc::new(AppState {
        config: GatewayConfig::load(None).unwrap().into(),
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
        reloader: None,
        analytics_pool: None,
    });

    let response = build_router(state)
        .oneshot(Request::builder().uri("/v1/models").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let data = json["data"].as_array().unwrap();
    let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"mock/llama3:8b") && ids.contains(&"mock/qwen3:4b"), "{ids:?}");

    for m in data {
        let caps = &m["capabilities"];
        assert!(caps.is_object(), "{} has capabilities {caps} (must be an object)", m["id"]);
        for flag in ["tools", "vision", "reasoning", "structured_output", "streaming", "embeddings"] {
            assert!(caps[flag].is_boolean(), "{}: capabilities.{flag} = {}", m["id"], caps[flag]);
        }
        // Only the adapter's own guarantee is claimed for discovered models.
        assert_eq!(caps["streaming"], true, "{}", m["id"]);
        assert_eq!(caps["tools"], false, "{}: unknown capabilities must not be over-claimed", m["id"]);
        assert_eq!(caps["vision"], false, "{}", m["id"]);
    }
}
