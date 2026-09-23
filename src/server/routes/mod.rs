pub mod analytics;
pub mod chat_completions;
pub mod embeddings;
pub mod health;
pub mod metrics;
pub mod models;
pub mod providers;
pub mod responses;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use super::AppState;

/// OpenAI-compatible API routes (/v1/...).
pub fn openai_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route("/v1/responses", post(responses::handle))
        .route("/v1/embeddings", post(embeddings::handle))
        .route("/v1/models", get(models::handle))
}

/// Management / operations routes.
pub fn management_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health::handle))
        .route("/providers", get(providers::handle))
        .route("/metrics", get(metrics::handle))
}

/// Read-only analytics query routes over the telemetry schema
/// (docs/REMEDIATION_PLAN.md Phase 4 item 1). Every handler degrades to
/// `503` if no database is configured/reachable — see
/// `analytics::require_pool`.
pub fn analytics_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/analytics/requests", get(analytics::list_requests))
        .route("/v1/analytics/requests/{request_id}", get(analytics::get_request))
        .route(
            "/v1/analytics/requests/{request_id}/timeline",
            get(analytics::get_request_timeline),
        )
        .route("/v1/analytics/providers/comparison", get(analytics::provider_comparison))
        .route("/v1/analytics/fallbacks", get(analytics::fallback_analysis))
}
