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

// `Response` is ~128 bytes, tripping clippy's `result_large_err` — not worth
// boxing across this function's 8 call sites for a value returned once per
// incoming HTTP request, not in a hot per-token loop.
#[allow(clippy::result_large_err)]
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

fn bad_request(code: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": { "message": message, "type": code, "code": code }
        })),
    )
        .into_response()
}

/// Encodes a keyset-pagination cursor from the last row on a page. Not
/// cryptographically opaque — just a stable, structured token a caller
/// round-trips verbatim rather than a positional offset. `timestamp_micros`
/// preserves full precision so two requests within the same microsecond
/// still keyset-paginate correctly (ties are broken by `request_id`, see
/// `list_requests`'s `ORDER BY`).
fn encode_cursor(received_at: chrono::DateTime<chrono::Utc>, request_id: Uuid) -> String {
    format!("{}_{}", received_at.timestamp_micros(), request_id)
}

/// Decodes a cursor produced by `encode_cursor`. `None` means the cursor
/// is malformed — the caller should treat this as a client error, not
/// silently fall back to an unfiltered first page.
fn decode_cursor(cursor: &str) -> Option<(chrono::DateTime<chrono::Utc>, Uuid)> {
    let (micros_str, id_str) = cursor.split_once('_')?;
    let received_at = chrono::DateTime::from_timestamp_micros(micros_str.parse().ok()?)?;
    let request_id = Uuid::parse_str(id_str).ok()?;
    Some((received_at, request_id))
}

#[derive(Debug, Deserialize)]
pub struct ListRequestsQuery {
    /// Max rows to return. Defaults to 50, capped at 500 to keep this a
    /// query endpoint, not an unbounded export.
    limit: Option<i64>,
    /// Opaque cursor from a previous response's `next_cursor`. Omit for
    /// the first page.
    cursor: Option<String>,
    /// Inclusive lower bound on `received_at` (UTC).
    from: Option<chrono::DateTime<chrono::Utc>>,
    /// Exclusive upper bound on `received_at` (UTC).
    to: Option<chrono::DateTime<chrono::Utc>>,
    /// Filter to a single requested/resolved model name (exact match).
    model: Option<String>,
    /// Filter to a single *serving* (final) provider (exact match) — see
    /// `attempted_provider` for the distinct "was this provider tried at
    /// all" filter.
    provider: Option<String>,
    /// Filter to requests where at least one attempt targeted this
    /// provider, regardless of whether that attempt succeeded or the
    /// request was ultimately served by it. Distinct from `provider`
    /// (docs/frontend/API_GAPS.md item 6: "an attempted-provider filter
    /// must be separate from serving-provider").
    attempted_provider: Option<String>,
    /// Filter to a single `application` tag (exact match).
    application: Option<String>,
    /// Filter to a single `task` tag (exact match).
    task: Option<String>,
    /// Filter to requests carrying this exact tag (from `request_tags`).
    tag: Option<String>,
    /// Filter to `true`/`false` — successful vs failed requests only.
    success: Option<bool>,
}

/// GET /v1/analytics/requests
///
/// Lists recent inference requests with optional filters, using stable
/// keyset pagination — not offset pagination. Ordered by
/// `(received_at DESC, request_id DESC)`; the previous `received_at DESC`
/// only ordering, combined with `LIMIT/OFFSET`, could duplicate or omit
/// rows when new requests were inserted between two page fetches (a real
/// correctness bug, not just a nicety — docs/frontend/API_GAPS.md item 6).
/// A keyset cursor built from the last row's own `(received_at,
/// request_id)` has no such gap: it names a stable position in the data,
/// not a row count that shifts under concurrent inserts.
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListRequestsQuery>,
) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let cursor = match q.cursor.as_deref() {
        Some(c) => match decode_cursor(c) {
            Some(decoded) => Some(decoded),
            None => return bad_request("invalid_cursor", "`cursor` is not a valid cursor from a previous response"),
        },
        None => None,
    };
    let (cursor_received_at, cursor_request_id) = match cursor {
        Some((ts, id)) => (Some(ts), Some(id)),
        None => (None, None),
    };

    let as_of = chrono::Utc::now();

    // Fetch one extra row to know whether a next page exists, without a
    // separate COUNT query.
    let fetch_limit = limit + 1;
    let rows: Vec<RequestSummaryRow> = match sqlx::query_as(
        r#"
        SELECT r.request_id, r.client_correlation_id, r.requested_model, r.resolved_model,
               r.serving_provider, r.success, r.streaming, r.total_latency_us, r.received_at
        FROM inference_requests r
        WHERE ($1::text IS NULL OR r.requested_model = $1 OR r.resolved_model = $1)
          AND ($2::text IS NULL OR r.serving_provider = $2)
          AND ($3::bool IS NULL OR r.success = $3)
          AND ($4::timestamptz IS NULL OR r.received_at >= $4)
          AND ($5::timestamptz IS NULL OR r.received_at < $5)
          AND ($6::text IS NULL OR r.application = $6)
          AND ($7::text IS NULL OR r.task = $7)
          AND ($8::text IS NULL OR EXISTS (
                SELECT 1 FROM request_tags t WHERE t.request_id = r.request_id AND t.tag = $8
              ))
          AND ($9::text IS NULL OR EXISTS (
                SELECT 1 FROM provider_attempts p WHERE p.request_id = r.request_id AND p.provider = $9
              ))
          AND ($10::timestamptz IS NULL OR (r.received_at, r.request_id) < ($10, $11))
        ORDER BY r.received_at DESC, r.request_id DESC
        LIMIT $12
        "#,
    )
    .bind(&q.model)
    .bind(&q.provider)
    .bind(q.success)
    .bind(q.from)
    .bind(q.to)
    .bind(&q.application)
    .bind(&q.task)
    .bind(&q.tag)
    .bind(&q.attempted_provider)
    .bind(cursor_received_at)
    .bind(cursor_request_id)
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    let has_more = rows.len() as i64 > limit;
    let page: Vec<RequestSummaryRow> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|r| encode_cursor(r.received_at, r.request_id))
    } else {
        None
    };

    Json(serde_json::json!({
        "requests": page,
        "next_cursor": next_cursor,
        // The server-side time this query ran — pairs with a `from`-bounded
        // follow-up query to answer "has anything new arrived since I last
        // looked" without re-deriving it from the page's own rows.
        "as_of": as_of,
        "limit": limit,
    }))
    .into_response()
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
               first_byte_at, first_token_at, synthetic_timing,
               timestamp AS started_at, finished_at
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
    // `duration_definition` isn't a column — it's the same fixed caveat for
    // every attempt, injected here rather than repeated in the DB (Audit
    // A06 / docs/frontend/API_GAPS.md item 5: "do not infer TCP connection
    // time from response-header time" — this states plainly what
    // `duration_us`/`finished_at` actually measure instead of leaving the
    // caller to assume dispatch-to-completion latency).
    let attempts: Vec<serde_json::Value> = attempts
        .into_iter()
        .map(|a| {
            let mut v = serde_json::to_value(&a).unwrap_or(serde_json::Value::Null);
            v["duration_definition"] = serde_json::json!(
                "duration_us/finished_at span the time from provider dispatch (Started) to this \
                 attempt's terminal event (Completed/Error). For a streaming attempt this is \
                 measured after InstrumentedStream fully drains the response, not at TCP \
                 connection or response-header time — see docs/ROUTING_AND_POLICIES.md's Stage 3 \
                 notes and docs/telemetry writer.rs's Connected event doc comment."
            );
            v
        })
        .collect();

    let generations: Vec<GenerationRow> = match sqlx::query_as(
        r#"
        SELECT id, attempt_number, choice_index, content, reasoning_content,
               reasoning_visibility, tool_calls, status
        FROM generations WHERE request_id = $1 ORDER BY attempt_number ASC, choice_index ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(g) => g,
        Err(e) => return query_error(e),
    };

    // Ordered by `id ASC` (insertion order) within each attempt so the
    // canonical-record computation below can treat "last in this list" as
    // "most recently observed" without a second query.
    let usage: Vec<UsageRow> = match sqlx::query_as(
        r#"
        SELECT id, attempt_number, timestamp AS observed_at, prompt_tokens, completion_tokens,
               total_tokens, cached_tokens, reasoning_tokens, source, kind
        FROM generation_usage WHERE request_id = $1 ORDER BY attempt_number ASC, id ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(u) => u,
        Err(e) => return query_error(e),
    };

    // An independently derived canonical record per attempt, plus a
    // quality flag — docs/frontend/API_GAPS.md item 2. Every provider this
    // gateway supports reports usage once per attempt as a cumulative
    // total (see `TelemetryEvent::UsageRecorded`'s doc comment), so under
    // normal operation each attempt has exactly one observation and
    // `quality` is `"single_observation"`. More than one observation for
    // the same attempt is not expected in normal operation, but is
    // detected and reported rather than silently resolved: identical
    // repeated values are `"duplicate_observations"` (e.g. a redelivered
    // telemetry event), and genuinely different values are
    // `"conflicting_observations"` — the canonical record for that case is
    // the most recently observed one, but the flag makes clear that a real
    // anomaly occurred rather than presenting it as ordinary.
    let mut usage_by_attempt: std::collections::BTreeMap<i32, Vec<&UsageRow>> = std::collections::BTreeMap::new();
    for row in &usage {
        usage_by_attempt.entry(row.attempt_number).or_default().push(row);
    }
    let usage_canonical: Vec<serde_json::Value> = usage_by_attempt
        .into_iter()
        .map(|(attempt_number, rows)| {
            let last = rows.last().unwrap();
            let all_identical = rows.iter().all(|r| {
                (r.prompt_tokens, r.completion_tokens, r.total_tokens, r.cached_tokens, r.reasoning_tokens)
                    == (last.prompt_tokens, last.completion_tokens, last.total_tokens, last.cached_tokens, last.reasoning_tokens)
            });
            let quality = if rows.len() == 1 {
                "single_observation"
            } else if all_identical {
                "duplicate_observations"
            } else {
                "conflicting_observations"
            };
            serde_json::json!({
                "attempt_number": attempt_number,
                "prompt_tokens": last.prompt_tokens,
                "completion_tokens": last.completion_tokens,
                "total_tokens": last.total_tokens,
                "cached_tokens": last.cached_tokens,
                "reasoning_tokens": last.reasoning_tokens,
                "observation_count": rows.len(),
                "quality": quality,
            })
        })
        .collect();

    // Routing rationale, structured errors, and cost provenance — all
    // previously omitted from request detail entirely
    // (docs/frontend/API_GAPS.md item 5).
    let routing: Vec<RoutingDecisionRow> = match sqlx::query_as(
        r#"
        SELECT timestamp, requested_model, resolution_type, candidates_considered,
               candidates_rejected, rejection_reasons, selected_provider, selected_model,
               policy_applied, short_circuit
        FROM routing_decisions WHERE request_id = $1 ORDER BY timestamp ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    let errors: Vec<ErrorRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, timestamp, error_code, error_message, status_code, provider, retryable
        FROM errors WHERE request_id = $1 ORDER BY timestamp ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(e) => e,
        Err(e) => return query_error(e),
    };

    let costs: Vec<CostRow> = match sqlx::query_as(
        r#"
        SELECT timestamp, estimated_cost, actual_cost, currency, provider, model, input_cost, output_cost
        FROM cost_records WHERE request_id = $1 ORDER BY timestamp ASC
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    {
        Ok(c) => c,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({
        "request": request,
        "attempts": attempts,
        "generations": generations,
        // Raw per-observation rows, preserved exactly as received.
        "usage": usage,
        // One independently derived record per attempt, plus a quality
        // flag — see the computation above and
        // docs/frontend/API_GAPS.md item 2.
        "usage_canonical": usage_canonical,
        "routing": routing,
        "errors": errors,
        "costs": costs,
    }))
    .into_response()
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct RoutingDecisionRow {
    timestamp: chrono::DateTime<chrono::Utc>,
    requested_model: String,
    resolution_type: String,
    candidates_considered: i32,
    candidates_rejected: i32,
    rejection_reasons: Option<serde_json::Value>,
    selected_provider: Option<String>,
    selected_model: Option<String>,
    policy_applied: String,
    short_circuit: Option<String>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct ErrorRow {
    attempt_number: Option<i32>,
    timestamp: chrono::DateTime<chrono::Utc>,
    error_code: String,
    error_message: Option<String>,
    status_code: Option<i32>,
    provider: Option<String>,
    retryable: Option<bool>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct CostRow {
    timestamp: chrono::DateTime<chrono::Utc>,
    estimated_cost: Option<f64>,
    actual_cost: Option<f64>,
    currency: String,
    provider: Option<String>,
    model: Option<String>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
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
    /// When this attempt was dispatched (`ProviderAttemptStarted`).
    started_at: chrono::DateTime<chrono::Utc>,
    /// When this attempt reached a terminal state (`ProviderAttemptCompleted`,
    /// success or failure alike). `None` for an attempt that never
    /// completed — e.g. the process was killed mid-attempt.
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct GenerationRow {
    /// Stable per-generation ID — the `docs/frontend/API_GAPS.md` item 5
    /// "generations include stable ID" ask. `generations.id` already
    /// existed as a primary key; it just wasn't exposed here before.
    id: i64,
    attempt_number: i32,
    /// Which choice this row belongs to. `0` for the overwhelmingly common
    /// single-choice case; distinct rows for `n > 1`'s other choices
    /// (migration `006_generation_choice_index.sql`).
    choice_index: i32,
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning_visibility: Option<String>,
    tool_calls: Option<serde_json::Value>,
    /// `"complete"` (reached via a genuine terminal event) or `"partial"`
    /// (reached via a mid-stream error with partial content preserved).
    status: String,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct UsageRow {
    id: i64,
    attempt_number: i32,
    observed_at: chrono::DateTime<chrono::Utc>,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    cached_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    /// Which code path produced this observation — see
    /// `TelemetryEvent::UsageRecorded`'s doc comment.
    source: Option<String>,
    /// Always `"cumulative"` for every provider this gateway currently
    /// supports — see `TelemetryEvent::UsageRecorded`'s doc comment.
    kind: String,
}

/// GET /v1/analytics/requests/:request_id/timeline
///
/// The chunk-level stream timeline for a request, ordered by sequence —
/// the raw material for TTFT/inter-chunk-latency reconstruction. Each row
/// carries `synthetic` so callers can tell fabricated (non-streaming,
/// reconstructed) timing apart from a genuine live stream themselves,
/// rather than this endpoint silently filtering either in or out
/// (docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06).
/// Encodes/decodes a timeline cursor from the `(attempt_number, sequence)`
/// of the last row on a page — stream events are append-only per attempt
/// (no equivalent of a request's concurrent-insert race, since one
/// request's events are only ever written by that one attempt's stream),
/// so this needs no tie-break beyond the natural ordering key itself.
fn encode_timeline_cursor(attempt_number: i32, sequence: i64) -> String {
    format!("{attempt_number}_{sequence}")
}

fn decode_timeline_cursor(cursor: &str) -> Option<(i32, i64)> {
    let (attempt_str, seq_str) = cursor.split_once('_')?;
    Some((attempt_str.parse().ok()?, seq_str.parse().ok()?))
}

#[derive(Debug, Deserialize)]
pub struct TimelineQuery {
    /// Opaque cursor from a previous response's `next_cursor`. Omit for
    /// the first page.
    after: Option<String>,
    /// Max events to return. Defaults to 1000, capped at 5000.
    limit: Option<i64>,
}

pub async fn get_request_timeline(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<Uuid>,
    Query(q): Query<TimelineQuery>,
) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let limit = q.limit.unwrap_or(1000).clamp(1, 5000);

    let cursor = match q.after.as_deref() {
        Some(c) => match decode_timeline_cursor(c) {
            Some(decoded) => Some(decoded),
            None => return bad_request("invalid_cursor", "`after` is not a valid cursor from a previous response"),
        },
        None => None,
    };
    let (after_attempt, after_seq) = match cursor {
        Some((a, s)) => (Some(a), Some(s)),
        None => (None, None),
    };

    // Fetch one extra row to know whether a next page exists.
    let fetch_limit = limit + 1;
    let rows: Vec<TimelineRow> = match sqlx::query_as(
        r#"
        SELECT attempt_number, timestamp, sequence, chunk_type, byte_count, synthetic
        FROM stream_events
        WHERE request_id = $1
          AND ($2::int IS NULL OR (attempt_number, sequence) > ($2, $3))
        ORDER BY attempt_number ASC, sequence ASC
        LIMIT $4
        "#,
    )
    .bind(request_id)
    .bind(after_attempt)
    .bind(after_seq)
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return query_error(e),
    };

    let has_more = rows.len() as i64 > limit;
    let page: Vec<TimelineRow> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|r| encode_timeline_cursor(r.attempt_number, r.sequence))
    } else {
        None
    };

    Json(serde_json::json!({
        "request_id": request_id,
        "events": page,
        "next_cursor": next_cursor,
        // `true` once a traversal has reached the last page — i.e. every
        // original event, across however many `after`-chained calls it
        // took, has now been returned exactly once
        // (docs/frontend/API_GAPS.md item 5's pagination acceptance
        // criterion).
        "complete": !has_more,
    }))
    .into_response()
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
/// `reported_cost` sums `cost_records.actual_cost`, the cost a provider put in
/// its own usage object (OpenRouter's `usage.cost`). `billed_cost` is always
/// `null`: neither an estimate nor a provider's per-response figure is an
/// invoice, and this endpoint does not pretend otherwise.
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

    let (estimated_token_charge, priced_attempts, reported_cost, reported_attempts): (Option<f64>, i64, Option<f64>, i64) = match sqlx::query_as(
        r#"
        SELECT SUM(c.estimated_cost), COUNT(c.estimated_cost), SUM(c.actual_cost), COUNT(c.actual_cost)
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
        SELECT c.provider, c.model, COUNT(c.estimated_cost) AS priced_attempts,
               SUM(c.estimated_cost) AS estimated_cost,
               COUNT(c.actual_cost) AS reported_attempts, SUM(c.actual_cost) AS reported_cost,
               SUM(c.input_cost) AS input_cost, SUM(c.output_cost) AS output_cost
        FROM cost_records c
        WHERE ($1::text IS NULL OR c.provider = $1)
          AND ($2::text IS NULL OR c.model = $2)
        GROUP BY c.provider, c.model
        ORDER BY COALESCE(SUM(c.actual_cost), SUM(c.estimated_cost)) DESC NULLS LAST
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
        // What providers said these requests cost, from their own usage
        // objects (OpenRouter's `usage.cost`). Covers only the attempts in
        // `reported_attempts`; providers that report nothing are absent from
        // it, not counted as free.
        "reported_cost": reported_cost,
        "reported_attempts": reported_attempts,
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
    reported_attempts: i64,
    reported_cost: Option<f64>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
}

/// A bucketing granularity for `GET /v1/analytics/summary`, mapped to a
/// Postgres `date_trunc` unit and a fixed interval. Only these three exist
/// — arbitrary custom durations aren't supported, since bucket boundaries
/// must land on stable, predictable UTC unit boundaries (top of the
/// minute/hour/day) for a caller to reason about, not on `from`-relative
/// offsets (docs/frontend/API_GAPS.md item 1).
#[derive(Debug, Clone, Copy)]
enum SummaryBucket {
    Minute,
    Hour,
    Day,
}

impl SummaryBucket {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "minute" => Some(Self::Minute),
            "hour" => Some(Self::Hour),
            "day" => Some(Self::Day),
            _ => None,
        }
    }

    fn duration(self) -> chrono::Duration {
        match self {
            Self::Minute => chrono::Duration::minutes(1),
            Self::Hour => chrono::Duration::hours(1),
            Self::Day => chrono::Duration::days(1),
        }
    }

    /// Used only to pick a default `from` when the caller omits it —
    /// roughly "a chart's worth" of history for that granularity.
    fn default_lookback(self) -> chrono::Duration {
        match self {
            Self::Minute => chrono::Duration::minutes(60),
            Self::Hour => chrono::Duration::hours(24),
            Self::Day => chrono::Duration::days(30),
        }
    }

    fn date_trunc_unit(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    fn pg_interval(self) -> &'static str {
        match self {
            Self::Minute => "1 minute",
            Self::Hour => "1 hour",
            Self::Day => "1 day",
        }
    }
}

const SUMMARY_MAX_BUCKETS: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct SummaryQuery {
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
    bucket: Option<String>,
    model: Option<String>,
    provider: Option<String>,
}

/// GET /v1/analytics/summary
///
/// A date-aware activity/usage summary over `[from, to)` (half-open — a
/// request landing exactly on `to` belongs to the *next* window, never
/// double-counted across adjacent calls), bucketed at `minute`, `hour`, or
/// `day` granularity with stable UTC unit boundaries. Answers "how much
/// traffic, how much of it succeeded, how much token usage" for a given
/// window and its time series — the "historical summary and series" gap
/// identified in docs/frontend/API_GAPS.md item 1.
///
/// `from`/`to` are RFC3339 timestamps. A UTC offset written as `+00:00`
/// contains a literal `+`, which standard query-string form-encoding
/// decodes as a space — percent-encode it (`%2B`) or, simpler, use the `Z`
/// suffix form instead. A well-behaved HTTP client library handles this
/// automatically; a hand-constructed URL will not.
///
/// `provider` filters on `serving_provider` — the *final* provider that
/// actually served the request, never an attempted-but-failed one (see
/// `scope.provider_semantics` in the response, which states this
/// explicitly rather than leaving it implicit). `model` filters on
/// `requested_model` OR `resolved_model`, matching this API's existing
/// convention elsewhere (`list_requests`).
///
/// Every count in `requests`/`attempts` reflects rows actually present —
/// `pending` means `completed_at IS NULL` (received but no `RequestCompleted`
/// telemetry event has landed yet, whether still in flight or lost), kept
/// distinct from `failed` (`success = false`, a genuine terminal failure).
/// `usage` fields are `null`, not `0`, when no usage was ever recorded in
/// that scope — a `UsageRecorded` event only fires when a provider actually
/// reports token counts, so absence is a real "unknown," not a measured
/// zero. Usage is de-duplicated to one canonical (most-recently-observed)
/// row per attempt before summing, same as `/providers/comparison`, so an
/// attempt with multiple cumulative usage updates is never double-counted.
///
/// The range is capped at `SUMMARY_MAX_BUCKETS` (500) buckets — a caller
/// requesting more must narrow the range or choose a coarser bucket; this
/// keeps the endpoint a bounded query, not an unbounded full-history scan.
pub async fn summary(State(state): State<Arc<AppState>>, Query(q): Query<SummaryQuery>) -> Response {
    let pool = match require_pool(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };

    let bucket_str = q.bucket.as_deref().unwrap_or("hour");
    let bucket = match SummaryBucket::parse(bucket_str) {
        Some(b) => b,
        None => return bad_request("invalid_bucket", "`bucket` must be one of: minute, hour, day"),
    };

    let to = q.to.unwrap_or_else(chrono::Utc::now);
    let from = q.from.unwrap_or_else(|| to - bucket.default_lookback());

    if from >= to {
        return bad_request("invalid_range", "`from` must be strictly before `to`");
    }

    let bucket_seconds = bucket.duration().num_seconds() as f64;
    let bucket_count = (((to - from).num_seconds() as f64) / bucket_seconds).ceil() as i64;
    if bucket_count > SUMMARY_MAX_BUCKETS {
        return bad_request(
            "range_too_large",
            &format!(
                "the requested range spans {bucket_count} `{bucket_str}` buckets, exceeding the {SUMMARY_MAX_BUCKETS} maximum — narrow the range or choose a coarser bucket"
            ),
        );
    }

    let totals: SummaryTotalsRow = match sqlx::query_as(
        r#"
        SELECT
            COUNT(*) AS requests_total,
            COUNT(*) FILTER (WHERE success = true) AS requests_successful,
            COUNT(*) FILTER (WHERE success = false) AS requests_failed,
            COUNT(*) FILTER (WHERE completed_at IS NULL) AS requests_pending
        FROM inference_requests
        WHERE received_at >= $1 AND received_at < $2
          AND ($3::text IS NULL OR requested_model = $3 OR resolved_model = $3)
          AND ($4::text IS NULL OR serving_provider = $4)
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&q.model)
    .bind(&q.provider)
    .fetch_one(pool)
    .await
    {
        Ok(t) => t,
        Err(e) => return query_error(e),
    };

    let attempt_totals: SummaryAttemptTotalsRow = match sqlx::query_as(
        r#"
        SELECT
            COUNT(*) AS attempts_total,
            COUNT(*) FILTER (WHERE p.status != 'success') AS attempts_failed
        FROM provider_attempts p
        JOIN inference_requests r ON r.request_id = p.request_id
        WHERE r.received_at >= $1 AND r.received_at < $2
          AND ($3::text IS NULL OR r.requested_model = $3 OR r.resolved_model = $3)
          AND ($4::text IS NULL OR r.serving_provider = $4)
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&q.model)
    .bind(&q.provider)
    .fetch_one(pool)
    .await
    {
        Ok(t) => t,
        Err(e) => return query_error(e),
    };

    let usage_totals: SummaryUsageRow = match sqlx::query_as(
        r#"
        WITH canonical_usage AS (
            SELECT DISTINCT ON (gu.request_id, gu.attempt_number)
                gu.request_id, gu.prompt_tokens, gu.completion_tokens, gu.cached_tokens, gu.reasoning_tokens
            FROM generation_usage gu
            JOIN inference_requests r ON r.request_id = gu.request_id
            WHERE r.received_at >= $1 AND r.received_at < $2
              AND ($3::text IS NULL OR r.requested_model = $3 OR r.resolved_model = $3)
              AND ($4::text IS NULL OR r.serving_provider = $4)
            ORDER BY gu.request_id, gu.attempt_number, gu.id DESC
        )
        SELECT
            SUM(prompt_tokens)::bigint AS prompt,
            SUM(completion_tokens)::bigint AS completion,
            SUM(cached_tokens)::bigint AS cached,
            SUM(reasoning_tokens)::bigint AS reasoning,
            COUNT(DISTINCT request_id) AS known_requests
        FROM canonical_usage
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&q.model)
    .bind(&q.provider)
    .fetch_one(pool)
    .await
    {
        Ok(u) => u,
        Err(e) => return query_error(e),
    };

    // The upper series bound is nudged back by one microsecond so a `to`
    // that lands exactly on a bucket boundary doesn't generate a spurious
    // final empty bucket starting at `to` itself — buckets only ever cover
    // time strictly before `to`, matching the half-open `[from, to)` scope.
    let series_rows: Vec<SummarySeriesRow> = match sqlx::query_as(
        r#"
        WITH buckets AS (
            SELECT generate_series(
                date_trunc($5, $1::timestamptz AT TIME ZONE 'UTC') AT TIME ZONE 'UTC',
                date_trunc($5, ($2::timestamptz - interval '1 microsecond') AT TIME ZONE 'UTC') AT TIME ZONE 'UTC',
                $6::interval
            ) AS bucket_start
        ),
        req_agg AS (
            SELECT
                date_trunc($5, received_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS bucket_start,
                COUNT(*) AS total,
                COUNT(*) FILTER (WHERE success = true) AS successful,
                COUNT(*) FILTER (WHERE success = false) AS failed,
                COUNT(*) FILTER (WHERE completed_at IS NULL) AS pending
            FROM inference_requests
            WHERE received_at >= $1 AND received_at < $2
              AND ($3::text IS NULL OR requested_model = $3 OR resolved_model = $3)
              AND ($4::text IS NULL OR serving_provider = $4)
            GROUP BY 1
        ),
        attempt_agg AS (
            SELECT
                date_trunc($5, r.received_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS bucket_start,
                COUNT(*) AS total,
                COUNT(*) FILTER (WHERE p.status != 'success') AS failed
            FROM provider_attempts p
            JOIN inference_requests r ON r.request_id = p.request_id
            WHERE r.received_at >= $1 AND r.received_at < $2
              AND ($3::text IS NULL OR r.requested_model = $3 OR r.resolved_model = $3)
              AND ($4::text IS NULL OR r.serving_provider = $4)
            GROUP BY 1
        ),
        canonical_usage AS (
            SELECT DISTINCT ON (gu.request_id, gu.attempt_number)
                gu.request_id, r.received_at AS received_at,
                gu.prompt_tokens, gu.completion_tokens, gu.cached_tokens, gu.reasoning_tokens
            FROM generation_usage gu
            JOIN inference_requests r ON r.request_id = gu.request_id
            WHERE r.received_at >= $1 AND r.received_at < $2
              AND ($3::text IS NULL OR r.requested_model = $3 OR r.resolved_model = $3)
              AND ($4::text IS NULL OR r.serving_provider = $4)
            ORDER BY gu.request_id, gu.attempt_number, gu.id DESC
        ),
        usage_agg AS (
            SELECT
                date_trunc($5, received_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS bucket_start,
                SUM(prompt_tokens)::bigint AS prompt,
                SUM(completion_tokens)::bigint AS completion,
                SUM(cached_tokens)::bigint AS cached,
                SUM(reasoning_tokens)::bigint AS reasoning,
                COUNT(DISTINCT request_id) AS known_requests
            FROM canonical_usage
            GROUP BY 1
        )
        SELECT
            b.bucket_start,
            COALESCE(ra.total, 0) AS requests_total,
            COALESCE(ra.successful, 0) AS requests_successful,
            COALESCE(ra.failed, 0) AS requests_failed,
            COALESCE(ra.pending, 0) AS requests_pending,
            COALESCE(aa.total, 0) AS attempts_total,
            COALESCE(aa.failed, 0) AS attempts_failed,
            ua.prompt AS usage_prompt,
            ua.completion AS usage_completion,
            ua.cached AS usage_cached,
            ua.reasoning AS usage_reasoning,
            COALESCE(ua.known_requests, 0) AS usage_known_requests
        FROM buckets b
        LEFT JOIN req_agg ra ON ra.bucket_start = b.bucket_start
        LEFT JOIN attempt_agg aa ON aa.bucket_start = b.bucket_start
        LEFT JOIN usage_agg ua ON ua.bucket_start = b.bucket_start
        ORDER BY b.bucket_start
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&q.model)
    .bind(&q.provider)
    .bind(bucket.date_trunc_unit())
    .bind(bucket.pg_interval())
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => return query_error(e),
    };

    Json(serde_json::json!({
        "scope": {
            "from": from,
            "to": to,
            "bucket": bucket_str,
            "provider_semantics": "serving_provider (the final provider that actually served the request; an attempted-but-failed provider never matches this filter)",
        },
        "requests": {
            "total": totals.requests_total,
            "successful": totals.requests_successful,
            "failed": totals.requests_failed,
            "pending": totals.requests_pending,
        },
        "attempts": {
            "total": attempt_totals.attempts_total,
            "failed": attempt_totals.attempts_failed,
        },
        "usage": {
            "prompt": usage_totals.prompt,
            "completion": usage_totals.completion,
            "cached": usage_totals.cached,
            "reasoning": usage_totals.reasoning,
            "known_requests": usage_totals.known_requests,
            "eligible_requests": totals.requests_total,
        },
        "series": series_rows,
    }))
    .into_response()
}

#[derive(Debug, sqlx::FromRow)]
struct SummaryTotalsRow {
    requests_total: i64,
    requests_successful: i64,
    requests_failed: i64,
    requests_pending: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct SummaryAttemptTotalsRow {
    attempts_total: i64,
    attempts_failed: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct SummaryUsageRow {
    prompt: Option<i64>,
    completion: Option<i64>,
    cached: Option<i64>,
    reasoning: Option<i64>,
    known_requests: i64,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct SummarySeriesRow {
    bucket_start: chrono::DateTime<chrono::Utc>,
    requests_total: i64,
    requests_successful: i64,
    requests_failed: i64,
    requests_pending: i64,
    attempts_total: i64,
    attempts_failed: i64,
    usage_prompt: Option<i64>,
    usage_completion: Option<i64>,
    usage_cached: Option<i64>,
    usage_reasoning: Option<i64>,
    usage_known_requests: i64,
}

/// GET /v1/analytics/persistence
///
/// Is the telemetry writer actually flushing, how far behind is it, and how
/// many events have been dropped or are sitting in the local fallback log —
/// distinct from `GET /health`, which reports provider status and says
/// nothing about whether data capture is actually working
/// (docs/frontend/API_GAPS.md item 8, persistence-health half; the auth
/// half is explicitly out of scope for this personal, loopback-only
/// deployment per the user's direction).
///
/// Unlike every other `/v1/analytics/*` endpoint, this one does **not**
/// require `analytics_pool` — it reports on the write-side pipeline, which
/// is independent of the read-side query pool (a deployment could have a
/// working writer and a broken analytics reader, or vice versa; conflating
/// them here would misrepresent one from the other's state).
pub async fn persistence_health(State(state): State<Arc<AppState>>) -> Response {
    let snapshot = state.telemetry_health.snapshot();

    // The channel between route handlers and the background writer is the
    // most direct "how far behind" signal available: its current backlog,
    // read directly from the `mpsc::Sender` rather than tracked
    // separately, so this can never drift out of sync with reality.
    let channel_capacity = state.telemetry_tx.max_capacity();
    let channel_free = state.telemetry_tx.capacity();
    let channel_pending = channel_capacity.saturating_sub(channel_free);

    // Honest, narrow definition: healthy means telemetry is actually
    // landing in the database right now, not just "not crashing". Disabled
    // and unavailable are both data-capture problems worth surfacing
    // plainly, not statuses to call healthy just because nothing is
    // erroring loudly.
    let healthy = snapshot.mode == "database" && snapshot.last_flush_ok && !snapshot.fallback_pending;

    Json(serde_json::json!({
        "mode": snapshot.mode,
        "healthy": healthy,
        "channel": {
            "capacity": channel_capacity,
            "pending": channel_pending,
        },
        "last_flush_at": snapshot.last_flush_at,
        "last_flush_ok": snapshot.last_flush_ok,
        "consecutive_flush_failures": snapshot.consecutive_flush_failures,
        "fallback_log_pending": snapshot.fallback_pending,
        "dropped_events_total": snapshot.dropped_events,
    }))
    .into_response()
}
