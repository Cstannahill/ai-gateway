use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};

use crate::server::AppState;

/// GET /providers
///
/// Returns status of all registered providers.
pub async fn handle(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let statuses = state.provider_registry.all_statuses();

    Json(serde_json::json!({
        "providers": statuses,
    }))
}
