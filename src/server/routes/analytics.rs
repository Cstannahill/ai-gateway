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
/// Per-provider/model aggregate stats: attempt volume/success, average
/// latency, average tokens/sec, and TTFT (mean + p50/p95) — restricted to
/// attempts with genuine (non-fabricated) timing, per the same
/// `synthetic_timing` exclusion documented in docs/TELEMETRY_AND_DATABASE.md.
/// Every average carries its own sample count, and TTFT semantics are
/// stated explicitly: it is measured from `provider_attempts.timestamp`,
/// includes failed attempts that still produced a first-token marker
/// (a deliberate choice — TTFT is a dispatch/reachability signal, not a
/// success-only metric), and excludes synthetic (non-streaming-reconstructed)
/// timing. See `docs/frontend/API_GAPS.md` item 3 and
/// `docs/frontend/METRICS.md` for the frontend contract this exists to serve.
pub async fn provider_comparison(State(state): State<Arc<AppState>>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    // `generation_usage` can carry more than one row per (request_id,
    // attempt_number) — every `UsageRecorded` event inserts a new row
    // rather than upserting one, since there is no finality marker yet
    // distinguishing a delta/cumulative/final observation
    // (docs/frontend/API_GAPS.md item 2 tracks giving that its own
    // contract). Joining `provider_attempts` directly against
    // `generation_usage` therefore multiplied every attempt row by its
    // usage-row count, inflating `total_attempts`/`successful_attempts`
    // and skewing every average's weighting — a real correctness bug, not
    // just a frontend-contract gap. The `LATERAL` subquery picks exactly
    // one canonical usage row per attempt (the most recently inserted, by
    // `id`) before the join, so this query can never multiply an attempt.
    //
    // Postgres's `ROUND(numeric, int)` and `PERCENTILE_CONT` both return
    // `NUMERIC`, which sqlx can't decode directly into `f64` — every such
    // aggregate is cast to `double precision` explicitly so the row type
    // matches what `ProviderComparisonRow` declares.
    let rows: Vec<ProviderComparisonRow> = match sqlx::query_as(
        r#"
        SELECT
            p.provider,
            p.model,
            COUNT(*) AS total_attempts,
            COUNT(*) FILTER (WHERE p.status = 'success') AS successful_attempts,
            ROUND((AVG(p.duration_us) FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false) / 1000.0)::numeric, 2)::float8 AS avg_duration_ms,
            COUNT(*) FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false AND p.duration_us IS NOT NULL) AS duration_sample_count,
            ROUND(
                (AVG(u.completion_tokens * 1000000.0 / NULLIF(p.duration_us, 0))
                    FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false))::numeric,
                2
            )::float8 AS avg_tokens_per_sec,
            COUNT(*) FILTER (WHERE p.status = 'success' AND p.synthetic_timing = false AND u.completion_tokens IS NOT NULL AND p.duration_us IS NOT NULL) AS throughput_sample_count,
            ROUND(
                (AVG(EXTRACT(EPOCH FROM (p.first_token_at - p.timestamp)) * 1000.0)
                    FILTER (WHERE p.synthetic_timing = false AND p.first_token_at IS NOT NULL))::numeric,
                2
            )::float8 AS avg_ttft_ms,
            ROUND(
                (PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (p.first_token_at - p.timestamp)) * 1000.0)
                    FILTER (WHERE p.synthetic_timing = false AND p.first_token_at IS NOT NULL))::numeric,
                2
            )::float8 AS p50_ttft_ms,
            ROUND(
                (PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (p.first_token_at - p.timestamp)) * 1000.0)
                    FILTER (WHERE p.synthetic_timing = false AND p.first_token_at IS NOT NULL))::numeric,
                2
            )::float8 AS p95_ttft_ms,
            COUNT(*) FILTER (WHERE p.synthetic_timing = false AND p.first_token_at IS NOT NULL) AS ttft_sample_count,
            COUNT(*) FILTER (WHERE p.synthetic_timing = true) AS synthetic_attempts
        FROM provider_attempts p
        LEFT JOIN LATERAL (
            SELECT gu.completion_tokens
            FROM generation_usage gu
            WHERE gu.request_id = p.request_id AND gu.attempt_number = p.attempt_number
            ORDER BY gu.id DESC
            LIMIT 1
        ) u ON true
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
    /// All retained attempt rows, including ones with synthetic timing —
    /// see `synthetic_attempts` to know how many of these are synthetic.
    total_attempts: i64,
    successful_attempts: i64,
    avg_duration_ms: Option<f64>,
    duration_sample_count: i64,
    avg_tokens_per_sec: Option<f64>,
    throughput_sample_count: i64,
    /// Includes failed attempts that still produced a first-token marker —
    /// TTFT is a dispatch/reachability signal, not success-only.
    avg_ttft_ms: Option<f64>,
    p50_ttft_ms: Option<f64>,
    p95_ttft_ms: Option<f64>,
    ttft_sample_count: i64,
    synthetic_attempts: i64,
}

const FALLBACK_SAMPLE_LIMIT: i64 = 200;

/// GET /v1/analytics/fallbacks
///
/// Fallback frequency and root cause: for each of the most recent
/// successful requests that had at least one non-success attempt along the
/// way, the full ordered attempt chain (not just the failures) so a caller
/// can see the whole recovery sequence — same-provider retry, model switch,
/// or provider switch.
///
/// The 200-row cap applies to *distinct requests*, not joined attempt rows.
/// The previous version applied `LIMIT 200` after joining
/// `provider_attempts`, so a handful of requests with many failed attempts
/// could crowd out others from the sample despite the API describing
/// itself as request-scoped — a real correctness bug, not just a
/// documentation gap (docs/frontend/API_GAPS.md item 7). `truncated: true`
/// means there were at least `FALLBACK_SAMPLE_LIMIT` matching requests, so
/// older recovered requests exist beyond this sample; this endpoint does
/// not attempt to provide an overall fallback-rate denominator across all
/// requests — that needs its own summary contract (API_GAPS item 7's
/// "independent summary contract" note).
pub async fn fallback_analysis(State(state): State<Arc<AppState>>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let sample_scope = format!(
        "up to {FALLBACK_SAMPLE_LIMIT} most recent successful requests with at least one non-success attempt, ordered by received_at DESC"
    );

    // Step 1: pick the *requests*, not attempt rows — this is what makes
    // the 200-cap request-scoped instead of attempt-row-scoped.
    let candidates: Vec<(Uuid, Option<String>)> = match sqlx::query_as(
        r#"
        SELECT r.request_id, r.serving_provider
        FROM inference_requests r
        WHERE r.success = true
          AND EXISTS (
              SELECT 1 FROM provider_attempts p
              WHERE p.request_id = r.request_id AND p.status != 'success'
          )
        ORDER BY r.received_at DESC
        LIMIT $1
        "#,
    )
    .bind(FALLBACK_SAMPLE_LIMIT)
    .fetch_all(pool)
    .await
    {
        Ok(c) => c,
        Err(e) => return query_error(e),
    };

    if candidates.is_empty() {
        return Json(serde_json::json!({ "fallbacks": [], "sample_scope": sample_scope, "truncated": false })).into_response();
    }

    let truncated = candidates.len() as i64 >= FALLBACK_SAMPLE_LIMIT;
    let ordered_ids: Vec<Uuid> = candidates.iter().map(|(id, _)| *id).collect();
    let succeeded_provider: std::collections::HashMap<Uuid, Option<String>> = candidates.into_iter().collect();

    // Step 2: the full attempt chain for each selected request — including
    // its eventual successful attempt, not only the failures — so the
    // recovery sequence is fully reconstructable.
    let attempts: Vec<FallbackAttemptRow> = match sqlx::query_as(
        r#"
        SELECT request_id, attempt_number, timestamp, provider, model, status, error_code, error_message
        FROM provider_attempts
        WHERE request_id = ANY($1)
        ORDER BY request_id, attempt_number ASC
        "#,
    )
    .bind(&ordered_ids)
    .fetch_all(pool)
    .await
    {
        Ok(a) => a,
        Err(e) => return query_error(e),
    };

    let mut by_request: std::collections::HashMap<Uuid, Vec<FallbackAttemptRow>> = std::collections::HashMap::new();
    for attempt in attempts {
        by_request.entry(attempt.request_id).or_default().push(attempt);
    }

    // Preserve the original received_at-DESC ordering from step 1 rather
    // than whatever order the hash map iterates in.
    let fallbacks: Vec<serde_json::Value> = ordered_ids
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "request_id": id,
                "succeeded_provider": succeeded_provider.get(&id).cloned().flatten(),
                "attempts": by_request.remove(&id).unwrap_or_default(),
            })
        })
        .collect();

    Json(serde_json::json!({ "fallbacks": fallbacks, "sample_scope": sample_scope, "truncated": truncated })).into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct FallbackAttemptRow {
    #[serde(skip_serializing)]
    request_id: Uuid,
    attempt_number: i32,
    timestamp: chrono::DateTime<chrono::Utc>,
    provider: String,
    model: String,
    status: String,
    error_code: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CostsQuery {
    provider: Option<String>,
    model: Option<String>,
}

/// GET /v1/analytics/costs
///
/// Cost summary: total estimated charge, pricing coverage (how many
/// attempts had a configured price vs. not), and a per-provider/model
/// breakdown. `cost_records` previously had a writer (Phase 3.5) but no
/// reader at all — every cost ever computed was write-only, unreachable
/// from any API (docs/frontend/API_GAPS.md item 4).
///
/// `estimated_token_charge` sums `cost_records.estimated_cost`, which is
/// only ever computed from a configured `pricing` entry in
/// `config/gateway.yaml` against reported token usage — never inferred
/// from a provider's name, and never fabricated as free for local
/// providers. A missing price yields an *unpriced* attempt (excluded from
/// the sum), not a zero-cost one; an explicitly configured zero price is
/// priced (counted in `priced_attempts` with its zero contributing to the
/// sum) — `priced_attempts`/`unpriced_attempts`/`eligible_attempts` let a
/// caller tell these apart instead of a bare total implying full coverage.
/// `billed_cost` is always `null`: this gateway has no concept of an
/// actual provider invoice, only an estimate from configured prices —
/// `cost_records.actual_cost` exists in the schema but nothing populates
/// it, and this endpoint does not pretend otherwise.
///
/// This is a whole-history summary, not a bucketed time series — see
/// docs/frontend/API_GAPS.md item 1 for the (not yet implemented)
/// date-bucketed series endpoint this could eventually share machinery
/// with.
pub async fn cost_summary(State(state): State<Arc<AppState>>, Query(q): Query<CostsQuery>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let eligible_attempts: i64 = match sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM provider_attempts p
        WHERE ($1::text IS NULL OR p.provider = $1)
          AND ($2::text IS NULL OR p.model = $2)
        "#,
    )
    .bind(&q.provider)
    .bind(&q.model)
    .fetch_one(pool)
    .await
    {
        Ok(c) => c,
        Err(e) => return query_error(e),
    };

    let (estimated_token_charge, priced_attempts): (Option<f64>, i64) = match sqlx::query_as(
        r#"
        SELECT SUM(c.estimated_cost), COUNT(*)
        FROM cost_records c
        WHERE ($1::text IS NULL OR c.provider = $1)
          AND ($2::text IS NULL OR c.model = $2)
        "#,
    )
    .bind(&q.provider)
    .bind(&q.model)
    .fetch_one(pool)
    .await
    {
        Ok(t) => t,
        Err(e) => return query_error(e),
    };

    let breakdowns: Vec<CostBreakdownRow> = match sqlx::query_as(
        r#"
        SELECT c.provider, c.model, COUNT(*) AS priced_attempts,
               SUM(c.estimated_cost) AS estimated_cost,
               SUM(c.input_cost) AS input_cost, SUM(c.output_cost) AS output_cost
        FROM cost_records c
        WHERE ($1::text IS NULL OR c.provider = $1)
          AND ($2::text IS NULL OR c.model = $2)
        GROUP BY c.provider, c.model
        ORDER BY estimated_cost DESC NULLS LAST
        "#,
    )
    .bind(&q.provider)
    .bind(&q.model)
    .fetch_all(pool)
    .await
    {
        Ok(b) => b,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({
        // `writer.rs` always inserts the literal 'USD' regardless of
        // config — there is no per-deployment currency setting to read
        // back yet, so this reflects the actual current implementation
        // rather than a config value that doesn't exist.
        "currency": "USD",
        "estimated_token_charge": estimated_token_charge,
        "billed_cost": serde_json::Value::Null,
        "priced_attempts": priced_attempts,
        "unpriced_attempts": (eligible_attempts - priced_attempts).max(0),
        "eligible_attempts": eligible_attempts,
        "pricing_source": "config/gateway.yaml `pricing` table (static, unversioned per-deployment $/1K-token prices)",
        "breakdowns": breakdowns,
    }))
    .into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct CostBreakdownRow {
    provider: Option<String>,
    model: Option<String>,
    priced_attempts: i64,
    estimated_cost: Option<f64>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
}
