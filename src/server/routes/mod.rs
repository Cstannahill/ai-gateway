pub mod analytics;
pub mod assets;
pub mod chat_completions;
pub mod embeddings;
pub mod health;
pub mod llama_server;
pub mod metrics;
pub mod models;
pub mod providers;
pub mod responses;

use std::sync::Arc;

use axum::routing::{get, post, put};
use axum::Router;

use super::AppState;

/// OpenAI-compatible API routes (/v1/...).
pub fn openai_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions::handle))
        .route("/v1/completions", post(chat_completions::handle_completions))
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
        .route("/favicon.ico", get(assets::favicon))
        .route("/icon.png", get(assets::icon_png))
}

/// Read-only analytics query routes over the telemetry schema
/// (docs/REMEDIATION_PLAN.md Phase 4 item 1). Every handler degrades to
/// `503` if no database is configured/reachable — see
/// `analytics::require_pool`.
pub fn analytics_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/analytics/summary", get(analytics::summary))
        .route("/v1/analytics/requests", get(analytics::list_requests))
        .route("/v1/analytics/requests/{request_id}", get(analytics::get_request))
        .route(
            "/v1/analytics/requests/{request_id}/timeline",
            get(analytics::get_request_timeline),
        )
        .route("/v1/analytics/providers/comparison", get(analytics::provider_comparison))
        .route("/v1/analytics/fallbacks", get(analytics::fallback_analysis))
        .route("/v1/analytics/costs", get(analytics::cost_summary))
        .route("/v1/analytics/persistence", get(analytics::persistence_health))
}

/// Local llama-server management (`/llama-server/*`). Every route, reads
/// included, sits behind `llama_server::browser_guard` — see that module for
/// why (process launching + permissive CORS).
pub fn llama_server_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/llama-server/status", get(llama_server::status))
        .route("/llama-server/start", post(llama_server::start))
        .route("/llama-server/stop", post(llama_server::stop))
        .route("/llama-server/restart", post(llama_server::restart))
        .route("/llama-server/logs", get(llama_server::logs))
        .route(
            "/llama-server/models",
            get(llama_server::list_models).post(llama_server::create_model),
        )
        .route("/llama-server/models/scan", get(llama_server::scan_models))
        .route("/llama-server/models/validate", post(llama_server::validate_model))
        .route(
            "/llama-server/models/{id}",
            put(llama_server::update_model).delete(llama_server::delete_model),
        )
        .route(
            "/llama-server/defaults",
            get(llama_server::get_defaults).put(llama_server::set_defaults),
        )
        .route("/llama-server/args/parse", post(llama_server::parse_args))
        .route_layer(axum::middleware::from_fn_with_state(state, llama_server::browser_guard))
}
