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
