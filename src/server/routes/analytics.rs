// ─── Analytics API ────────────────────────────────────────────────────────────
//
// Read-only query endpoints over the telemetry schema — docs/REMEDIATION_PLAN.md
// Phase 4 item 1 ("a thin Rust layer over the now-trustworthy Postgres
// schema"). Deliberately scoped to what the plan calls for: request lookup,
// per-request timeline reconstruction, and cross-provider comparison
// queries. No dashboard, no mutation, no new tables — this is a query
// surface over data the rest of the gateway already writes.
//
// Every handler here needs `state.analytics_pool` to be `Some(..)`; if the
// gateway started without a reachable database, every route in this module
// returns 503 rather than panicking.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::server::AppState;

fn db_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "message": "Analytics endpoints require a reachable database (DATABASE_URL); none is configured or connected.",
                "type": "analytics_unavailable",
                "code": "analytics_unavailable",
            }
        })),
    )
        .into_response()
}

fn require_pool(state: &AppState) -> Result<&PgPool, Response> {
    state.analytics_pool.as_ref().ok_or_else(db_unavailable)
}

fn query_error(e: sqlx::Error) -> Response {
    tracing::error!(error = %e, "Analytics query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": { "message": "Analytics query failed", "type": "analytics_query_error", "code": "analytics_query_error" }
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ListRequestsQuery {
    /// Max rows to return. Defaults to 50, capped at 500 to keep this a
    /// query endpoint, not an unbounded export.
    limit: Option<i64>,
    offset: Option<i64>,
    /// Filter to a single requested/resolved model name (exact match).
    model: Option<String>,
    /// Filter to a single serving provider (exact match).
    provider: Option<String>,
    /// Filter to `true`/`false` — successful vs failed requests only.
    success: Option<bool>,
}

/// GET /v1/analytics/requests
///
/// Lists recent inference requests, newest first, with optional filters.
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListRequestsQuery>,
) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);

    let rows = sqlx::query_as::<_, RequestSummaryRow>(
        r#"
        SELECT request_id, client_correlation_id, requested_model, resolved_model,
               serving_provider, success, streaming, total_latency_us, received_at
        FROM inference_requests
        WHERE ($1::text IS NULL OR requested_model = $1 OR resolved_model = $1)
          AND ($2::text IS NULL OR serving_provider = $2)
          AND ($3::bool IS NULL OR success = $3)
        ORDER BY received_at DESC
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(&q.model)
    .bind(&q.provider)
    .bind(q.success)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await;

    match rows {
        Ok(rows) => Json(serde_json::json!({ "requests": rows, "limit": limit, "offset": offset })).into_response(),
        Err(e) => query_error(e),
    }
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct RequestSummaryRow {
    request_id: Uuid,
    client_correlation_id: Option<String>,
    requested_model: String,
    resolved_model: Option<String>,
    serving_provider: Option<String>,
    success: Option<bool>,
    streaming: Option<bool>,
    total_latency_us: Option<i64>,
    received_at: chrono::DateTime<chrono::Utc>,
}

/// GET /v1/analytics/requests/:request_id
///
/// Full detail for one request: the parent row, every provider attempt,
/// the generation (if any), and usage — everything needed to answer "what
/// actually happened for this request" without hand-writing a join.
pub async fn get_request(State(state): State<Arc<AppState>>, Path(request_id): Path<Uuid>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let request: Option<RequestDetailRow> = match sqlx::query_as(
        r#"
        SELECT request_id, client_correlation_id, requested_model, resolved_model,
               serving_provider, success, streaming, total_latency_us, received_at,
               canonical_messages, raw_request
        FROM inference_requests WHERE request_id = $1
        "#,
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    let Some(request) = request else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "message": "request not found", "type": "not_found", "code": "not_found" } })),
        )
            .into_response();
    };

    let attempts: Vec<AttemptRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, provider, model, status, finish_reason, duration_us,
               upstream_provider, provider_response_id, error_code, error_message,
               first_byte_at, first_token_at, synthetic_timing
        FROM provider_attempts WHERE request_id = $1 ORDER BY attempt_number ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(a) => a,
        Err(e) => return query_error(e),
    };

    let generations: Vec<GenerationRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, content, reasoning_content, reasoning_visibility, tool_calls
        FROM generations WHERE request_id = $1 ORDER BY attempt_number ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(g) => g,
        Err(e) => return query_error(e),
    };

    let usage: Vec<UsageRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, prompt_tokens, completion_tokens, total_tokens, cached_tokens, reasoning_tokens
        FROM generation_usage WHERE request_id = $1 ORDER BY attempt_number ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(u) => u,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({
        "request": request,
        "attempts": attempts,
        "generations": generations,
        "usage": usage,
    }))
    .into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct RequestDetailRow {
    request_id: Uuid,
    client_correlation_id: Option<String>,
    requested_model: String,
    resolved_model: Option<String>,
    serving_provider: Option<String>,
    success: Option<bool>,
    streaming: Option<bool>,
    total_latency_us: Option<i64>,
    received_at: chrono::DateTime<chrono::Utc>,
    canonical_messages: Option<serde_json::Value>,
    raw_request: Option<serde_json::Value>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct AttemptRow {
    attempt_number: i32,
    provider: String,
    model: String,
    status: String,
    finish_reason: Option<String>,
    duration_us: Option<i64>,
    upstream_provider: Option<String>,
    provider_response_id: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    first_byte_at: Option<chrono::DateTime<chrono::Utc>>,
    first_token_at: Option<chrono::DateTime<chrono::Utc>>,
    synthetic_timing: bool,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct GenerationRow {
    attempt_number: i32,
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning_visibility: Option<String>,
    tool_calls: Option<serde_json::Value>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct UsageRow {
    attempt_number: i32,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    cached_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
}

/// GET /v1/analytics/requests/:request_id/timeline
///
/// The chunk-level stream timeline for a request, ordered by sequence —
/// the raw material for TTFT/inter-chunk-latency reconstruction. Each row
/// carries `synthetic` so callers can tell fabricated (non-streaming,
/// reconstructed) timing apart from a genuine live stream themselves,
/// rather than this endpoint silently filtering either in or out
/// (docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06).
pub async fn get_request_timeline(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<Uuid>,
) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let rows: Vec<TimelineRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, timestamp, sequence, chunk_type, byte_count, synthetic
        FROM stream_events WHERE request_id = $1 ORDER BY attempt_number ASC, sequence ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({ "request_id": request_id, "events": rows })).into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct TimelineRow {
    attempt_number: i32,
    timestamp: chrono::DateTime<chrono::Utc>,
    sequence: i64,
    chunk_type: String,
    byte_count: Option<i32>,
    synthetic: bool,
}

/// GET /v1/analytics/providers/comparison
///
/// Per-provider/model aggregate stats: success rate, average latency,
/// average tokens/sec, and average TTFT — restricted to attempts with
/// genuine (non-fabricated) timing, per the same `synthetic_timing`
/// exclusion documented in docs/TELEMETRY_AND_DATABASE.md.
pub async fn provider_comparison(State(state): State<Arc<AppState>>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    // Postgres's `ROUND(numeric, int)` returns `NUMERIC`, which sqlx can't
    // decode directly into `f64` — every rounded aggregate is cast to
    // `double precision` explicitly so the row type matches what
    // `ProviderComparisonRow` declares.
    let rows: Vec<ProviderComparisonRow> = match sqlx::query_as(
        r#"
        SELECT
            p.provider,
            p.model,
            COUNT(*) AS total_attempts,
            COUNT(*) FILTER (WHERE p.status = 'success') AS successful_attempts,
            ROUND((AVG(p.duration_us) FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false) / 1000.0)::numeric, 2)::float8 AS avg_duration_ms,
            ROUND(
                (AVG(u.completion_tokens * 1000000.0 / NULLIF(p.duration_us, 0))
                    FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false))::numeric,
                2
            )::float8 AS avg_tokens_per_sec,
            ROUND(
                (AVG(EXTRACT(EPOCH FROM (p.first_token_at - p.timestamp)) * 1000.0)
                    FILTER (WHERE p.synthetic_timing = false AND p.first_token_at IS NOT NULL))::numeric,
                2
            )::float8 AS avg_ttft_ms
        FROM provider_attempts p
        LEFT JOIN generation_usage u ON p.request_id = u.request_id AND p.attempt_number = u.attempt_number
        GROUP BY p.provider, p.model
        ORDER BY p.provider, p.model
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({ "providers": rows })).into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct ProviderComparisonRow {
    provider: String,
    model: String,
    total_attempts: i64,
    successful_attempts: i64,
    avg_duration_ms: Option<f64>,
    avg_tokens_per_sec: Option<f64>,
    avg_ttft_ms: Option<f64>,
}

/// GET /v1/analytics/fallbacks
///
/// Fallback frequency and root cause: for each request whose ultimately
/// successful attempt wasn't the first, what earlier attempt(s) failed and
/// why.
pub async fn fallback_analysis(State(state): State<Arc<AppState>>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let rows: Vec<FallbackRow> = match sqlx::query_as(
        r#"
        SELECT
            r.request_id,
            r.serving_provider AS succeeded_provider,
            p.provider AS failed_provider,
            p.model AS failed_model,
            p.error_code,
            p.error_message
        FROM inference_requests r
        JOIN provider_attempts p ON r.request_id = p.request_id
        WHERE p.status != 'success'
          AND r.success = true
        ORDER BY r.received_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({ "fallbacks": rows })).into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct FallbackRow {
    request_id: Uuid,
    succeeded_provider: Option<String>,
    failed_provider: String,
    failed_model: String,
    error_code: Option<String>,
    error_message: Option<String>,
}
