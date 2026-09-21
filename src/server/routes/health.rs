use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};

use crate::server::AppState;

/// GET /health
///
/// Gateway health check endpoint.
pub async fn handle(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let statuses = state.provider_registry.all_statuses();

    let all_healthy = statuses.iter().all(|s| {
        s.health == crate::types::ProviderHealth::Healthy
            || s.health == crate::types::ProviderHealth::Unknown
    });

    Json(serde_json::json!({
        "status": if all_healthy { "healthy" } else { "degraded" },
        "gateway": "ai-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "providers": statuses.len(),
    }))
}
