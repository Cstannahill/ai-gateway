// ─── Server Module ────────────────────────────────────────────────────────────
//
// Axum application builder, route registration, and shared application state.

pub mod middleware;
pub mod routes;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::config::GatewayConfig;
use crate::registry::{ModelRegistry, ProviderRegistry};
use crate::routing::Router as RoutingEngine;
use crate::telemetry::TelemetryEvent;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub config: GatewayConfig,
    pub model_registry: ModelRegistry,
    pub provider_registry: ProviderRegistry,
    pub router: RoutingEngine,
    pub telemetry_tx: mpsc::Sender<TelemetryEvent>,
    pub start_time: Instant,
}

impl AppState {
    /// Send a telemetry event non-blockingly, logging a warning if the channel is full.
    pub fn emit_telemetry(&self, event: TelemetryEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.telemetry_tx.try_send(event) {
            tracing::warn!("Telemetry queue full; event dropped");
        }
    }
}

/// Build the Axum application with all routes and middleware layers.
pub fn build_router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.server.body_limit_bytes;

    Router::new()
        .merge(routes::openai_routes())
        .merge(routes::management_routes())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(middleware::UuidV7RequestId))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}
