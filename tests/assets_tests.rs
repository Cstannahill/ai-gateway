//! The gateway serves its application icon (embedded in the binary) so a
//! browser tab pointed at it shows the logo instead of a 404 favicon.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};

async fn get(path: &str) -> (StatusCode, Option<String>, Option<String>, Vec<u8>) {
    let (telemetry_tx, _rx) = tokio::sync::mpsc::channel(10);
    let state = Arc::new(AppState {
        config: GatewayConfig::load(None).unwrap(),
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
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
    let response = build_router(state)
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let (content_type, cache) = (header("content-type"), header("cache-control"));
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec();
    (status, content_type, cache, body)
}

#[tokio::test]
async fn favicon_is_a_multi_size_ico() {
    let (status, content_type, cache, body) = get("/favicon.ico").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/x-icon"));
    assert!(cache.unwrap().contains("max-age"), "icons are cacheable");
    // ICONDIR: reserved 0, type 1 (icon), then the number of images.
    assert_eq!(&body[..4], [0, 0, 1, 0], "not an .ico file");
    let images = u16::from_le_bytes([body[4], body[5]]);
    assert!(images >= 3, "expected 16/32/48px images, got {images}");
}

#[tokio::test]
async fn icon_png_is_a_square_png() {
    let (status, content_type, _cache, body) = get("/icon.png").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));
    assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    // IHDR width/height follow the 8-byte signature and 8 bytes of chunk header.
    let width = u32::from_be_bytes(body[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(body[20..24].try_into().unwrap());
    assert_eq!((width, height), (256, 256));
}
