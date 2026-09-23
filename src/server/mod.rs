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
    /// Count of incoming gateway requests across all endpoints — distinct
    /// from `ProviderRegistry`'s per-provider `total_requests`, which counts
    /// *provider attempts* (one incoming request can produce several, via
    /// retries/fallback). `/metrics.gateway.total_requests` previously
    /// reported the latter under a name implying the former
    /// (docs/REMEDIATION_PLAN.md Phase 3.3 / Audit A13).
    pub total_gateway_requests: std::sync::atomic::AtomicU64,
    /// Read-only pool for the analytics query endpoints
    /// (`/v1/analytics/*`), separate from `TelemetryWriter`'s own
    /// write-side pool. `None` when no `DATABASE_URL` is configured or the
    /// database was unreachable at startup — analytics endpoints return
    /// `503` rather than panicking (docs/REMEDIATION_PLAN.md Phase 4.1).
    pub analytics_pool: Option<sqlx::PgPool>,
}

impl AppState {
    /// Send a telemetry event non-blockingly, logging a warning if the channel is full.
    pub fn emit_telemetry(&self, event: TelemetryEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.telemetry_tx.try_send(event) {
            tracing::warn!("Telemetry queue full; event dropped");
        }
    }

    /// Record one incoming gateway request (call once per request, at the
    /// top of each route handler, regardless of how it's ultimately routed
    /// or whether it succeeds).
    pub fn record_gateway_request(&self) {
        self.total_gateway_requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn gateway_request_count(&self) -> u64 {
        self.total_gateway_requests.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Build the Axum application with all routes and middleware layers.
pub fn build_router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.server.body_limit_bytes;

    Router::new()
        .merge(routes::openai_routes())
        .merge(routes::management_routes())
        .merge(routes::analytics_routes())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(middleware::UuidV7RequestId))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}
