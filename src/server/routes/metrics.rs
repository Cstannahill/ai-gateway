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

    // `total_provider_attempts`/`total_provider_errors` count provider
    // attempts (one incoming request can produce several via retries and
    // fallback); `gateway.total_requests` below now counts actual incoming
    // requests via `AppState::record_gateway_request`, which previously
    // didn't exist — `/metrics.gateway.total_requests` used to report the
    // attempt count under a name implying the request count
    // (docs/REMEDIATION_PLAN.md Phase 3.3 / Audit A13).
    let total_provider_attempts: u64 = statuses.iter().map(|s| s.total_requests).sum();
    let total_provider_errors: u64 = statuses.iter().map(|s| s.total_errors).sum();
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
                "avg_health_check_latency_ms": s.avg_health_check_latency_ms,
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
            "total_requests": state.gateway_request_count(),
            "total_provider_attempts": total_provider_attempts,
            "total_provider_errors": total_provider_errors,
            "active_requests": active_requests,
        },
        "providers": per_provider,
    }))
}
