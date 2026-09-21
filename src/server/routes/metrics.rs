use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};

use crate::server::AppState;

/// GET /metrics
///
/// Returns gateway metrics in JSON format including live uptime,
/// request counts, error rates, and rolling latencies.
pub async fn handle(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let statuses = state.provider_registry.all_statuses();

    let total_requests: u64 = statuses.iter().map(|s| s.total_requests).sum();
    let total_errors: u64 = statuses.iter().map(|s| s.total_errors).sum();
    let active_requests: u32 = statuses.iter().map(|s| s.active_requests).sum();
    let uptime_seconds = state.start_time.elapsed().as_secs();

    let per_provider: Vec<serde_json::Value> = statuses
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "provider_type": s.provider_type,
                "enabled": s.enabled,
                "health": s.health,
                "total_requests": s.total_requests,
                "total_errors": s.total_errors,
                "active_requests": s.active_requests,
                "avg_latency_ms": s.avg_latency_ms,
                "error_rate": if s.total_requests > 0 {
                    s.total_errors as f64 / s.total_requests as f64
                } else {
                    0.0
                },
            })
        })
        .collect();

    Json(serde_json::json!({
        "gateway": {
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": uptime_seconds,
            "total_requests": total_requests,
            "total_errors": total_errors,
            "active_requests": active_requests,
        },
        "providers": per_provider,
    }))
}
