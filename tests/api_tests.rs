use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{build_router, AppState};

fn setup_test_app() -> axum::Router {
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
        analytics_pool: None,
    });

    build_router(state)
}

#[tokio::test]
async fn test_health_endpoint() {
    let app = setup_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
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
    assert_eq!(json["status"], "healthy");
    assert!(json.get("version").is_some());
    assert_eq!(json["gateway"], "ai-gateway");
}

#[tokio::test]
async fn test_metrics_endpoint() {
    let app = setup_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
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
    assert!(json["gateway"]["uptime_seconds"].is_number());
    assert!(json["gateway"]["total_requests"].is_number());
    assert!(json["providers"].is_array());
}

#[tokio::test]
async fn test_models_endpoint() {
    let app = setup_test_app();

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

#[tokio::test]
async fn test_providers_endpoint() {
    let app = setup_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/providers")
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
    assert!(json["providers"].is_array());
}

#[tokio::test]
async fn test_chat_completions_missing_body() {
    let app = setup_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Axum returns 400 or 422 on invalid/missing JSON body
    assert!(response.status().is_client_error());
}

#[tokio::test]
async fn test_gateway_request_counter_counts_actual_requests() {
    // `/metrics.gateway.total_requests` previously summed provider
    // *attempts*, not incoming requests, and excluded routing failures
    // (Audit A13). This counts a request that fails before any provider is
    // even contacted (no providers configured) and one that fails
    // validation, both of which must still be counted.
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
        analytics_pool: None,
    });

    assert_eq!(state.gateway_request_count(), 0);

    for body in [
        serde_json::json!({"model": "nonexistent", "messages": []}),
        serde_json::json!({"model": "nonexistent", "messages": []}),
    ] {
        let _ = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    assert_eq!(state.gateway_request_count(), 2);
}
