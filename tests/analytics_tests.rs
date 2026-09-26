//! End-to-end tests for the `/v1/analytics/*` read-only query API
//! (docs/REMEDIATION_PLAN.md Phase 4 item 1). Needs a real database, same
//! opt-in pattern as `tests/db_verification_tests.rs`:
//!
//!   docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16
//!   AUDIT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/postgres \
//!     cargo test --test analytics_tests

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use ai_gateway::config::{GatewayConfig, PricingEntry};
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{build_router, AppState};
use ai_gateway::telemetry::{TelemetryEvent, TelemetryWriter};
use ai_gateway::types::Usage;
use chrono::{TimeZone, Utc};
use uuid::Uuid;

async fn seeded_state_and_request_id() -> Option<(Arc<AppState>, Uuid)> {
    let url = std::env::var("AUDIT_DATABASE_URL").ok()?;

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: Some("client-corr-1".to_string()),
            application: None,
            task: None,
            tags: vec![],
            requested_model: "analytics-test-model".to_string(),
            streaming: true,
            canonical_messages: Some(serde_json::json!([{"role": "user", "content": "hi"}])),
            raw_request: Some(serde_json::json!({"model": "analytics-test-model"})),
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: "analytics-test-provider".to_string(),
            model: "analytics-test-model".to_string(),
        },
        TelemetryEvent::FirstProviderByte {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            synthetic: false,
        },
        TelemetryEvent::StreamChunk {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            sequence: 1,
            chunk_type: "content".to_string(),
            byte_count: 5,
            synthetic: false,
        },
        TelemetryEvent::GenerationContent {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            content: "hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            raw_response: None,
            choice_index: 0,
            status: "complete".to_string(),
        },
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Success,
            finish_reason: Some("stop".to_string()),
            provider_response_id: Some("resp-1".to_string()),
            upstream_provider: None,
            duration_us: 5000,
            error_code: None,
            error_message: None,
        },
        TelemetryEvent::RequestCompleted {
            request_id,
            timestamp: Utc::now(),
            total_latency_us: 6000,
            total_attempts: 1,
            serving_provider: Some("analytics-test-provider".to_string()),
            resolved_model: Some("analytics-test-model".to_string()),
            success: true,
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    Some((state, request_id))
}

async fn get(state: Arc<AppState>, path: &str) -> (StatusCode, serde_json::Value) {
    let response = build_router(state)
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn analytics_returns_503_without_a_database() {
    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
    });

    let (status, _) = get(state, "/v1/analytics/requests").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn analytics_requests_list_and_detail_reflect_seeded_data() {
    let Some((state, request_id)) = seeded_state_and_request_id().await else {
        eprintln!("skipping analytics_requests_list_and_detail_reflect_seeded_data: AUDIT_DATABASE_URL not set");
        return;
    };

    let (status, body) = get(state.clone(), "/v1/analytics/requests?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    let requests = body["requests"].as_array().unwrap();
    assert!(
        requests.iter().any(|r| r["request_id"] == request_id.to_string()),
        "seeded request not found in list: {body}"
    );

    let (status, body) = get(state.clone(), &format!("/v1/analytics/requests/{request_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["request"]["requested_model"], "analytics-test-model");
    assert_eq!(body["attempts"][0]["provider"], "analytics-test-provider");
    assert_eq!(body["generations"][0]["content"], "hello");
    assert_eq!(body["usage"][0]["prompt_tokens"], 10);

    let missing_id = Uuid::now_v7();
    let (status, _) = get(state, &format!("/v1/analytics/requests/{missing_id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn analytics_fallback_analysis_reflects_a_failed_then_succeeded_request() {
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_fallback_analysis_reflects_a_failed_then_succeeded_request: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: "fallback-test-model".to_string(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: "flaky-provider".to_string(),
            model: "fallback-test-model".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Failed,
            finish_reason: None,
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 100,
            error_code: Some("server_error".to_string()),
            error_message: Some("upstream 503".to_string()),
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 2,
            timestamp: Utc::now(),
            provider: "reliable-provider".to_string(),
            model: "fallback-test-model".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 2,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Success,
            finish_reason: Some("stop".to_string()),
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 200,
            error_code: None,
            error_message: None,
        },
        TelemetryEvent::RequestCompleted {
            request_id,
            timestamp: Utc::now(),
            total_latency_us: 300,
            total_attempts: 2,
            serving_provider: Some("reliable-provider".to_string()),
            resolved_model: Some("fallback-test-model".to_string()),
            success: true,
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, "/v1/analytics/fallbacks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["truncated"], false);
    assert!(body["sample_scope"].as_str().unwrap().contains("200"));
    let fallbacks = body["fallbacks"].as_array().unwrap();
    let entry = fallbacks
        .iter()
        .find(|f| f["request_id"] == request_id.to_string())
        .unwrap_or_else(|| panic!("seeded fallback not found: {body}"));
    assert_eq!(entry["succeeded_provider"], "reliable-provider");

    // The full attempt chain must be present, in order — not just the
    // failures (docs/frontend/API_GAPS.md item 7).
    let attempts = entry["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["provider"], "flaky-provider");
    assert_eq!(attempts[0]["status"], "failed");
    assert_eq!(attempts[0]["error_code"], "server_error");
    assert_eq!(attempts[1]["provider"], "reliable-provider");
    assert_eq!(attempts[1]["status"], "success");
}

#[tokio::test]
async fn analytics_timeline_and_provider_comparison_reflect_seeded_data() {
    let Some((state, request_id)) = seeded_state_and_request_id().await else {
        eprintln!("skipping analytics_timeline_and_provider_comparison_reflect_seeded_data: AUDIT_DATABASE_URL not set");
        return;
    };

    let (status, body) = get(state.clone(), &format!("/v1/analytics/requests/{request_id}/timeline")).await;
    assert_eq!(status, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["chunk_type"], "content");
    assert_eq!(events[0]["synthetic"], false);

    let (status, body) = get(state, "/v1/analytics/providers/comparison").await;
    assert_eq!(status, StatusCode::OK);
    let providers = body["providers"].as_array().unwrap();
    assert!(
        providers
            .iter()
            .any(|p| p["provider"] == "analytics-test-provider" && p["model"] == "analytics-test-model"),
        "seeded provider not found in comparison: {body}"
    );
}

#[tokio::test]
async fn analytics_provider_comparison_does_not_multiply_attempts_with_multiple_usage_rows() {
    // `generation_usage` can carry more than one row per attempt (every
    // `UsageRecorded` event inserts, never upserts) — the comparison query
    // previously joined directly against it, so an attempt with two usage
    // rows counted as two attempts and skewed every average's weighting
    // (docs/frontend/API_GAPS.md item 3).
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_provider_comparison_does_not_multiply_attempts_with_multiple_usage_rows: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    // Run-unique provider/model names, same convention used elsewhere in
    // this file — `provider_comparison` aggregates by provider+model, not
    // by request, so a static name here would collide with this same
    // test's own data from a prior run against a persistent (non-reset)
    // database and inflate the attempt count this test asserts on.
    let run_id = Uuid::now_v7();
    let provider_name = format!("dup-usage-provider-{run_id}");
    let model_name = format!("dup-usage-model-{run_id}");

    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: model_name.clone(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: provider_name.clone(),
            model: model_name.clone(),
        },
        // Two cumulative usage updates for the *same* attempt — a
        // realistic sequence, not a contrived edge case.
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Success,
            finish_reason: Some("stop".to_string()),
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 1_000_000,
            error_code: None,
            error_message: None,
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let usage_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM generation_usage WHERE request_id = $1")
        .bind(request_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usage_rows, 2, "test setup should have created two usage rows for one attempt");

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, "/v1/analytics/providers/comparison").await;
    assert_eq!(status, StatusCode::OK);
    let row = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == provider_name)
        .unwrap_or_else(|| panic!("seeded provider not found: {body}"));

    assert_eq!(row["total_attempts"], 1, "two usage rows must not multiply the attempt count");
    assert_eq!(row["successful_attempts"], 1);
    assert_eq!(row["throughput_sample_count"], 1);
    // Canonical usage is the most recently inserted row (completion_tokens
    // = 20), giving 20 tokens / 1 second = 20 tok/s — not some average or
    // sum across both usage rows.
    assert_eq!(row["avg_tokens_per_sec"], 20.0);
}

#[tokio::test]
async fn analytics_cost_summary_distinguishes_priced_from_unpriced_attempts() {
    // `cost_records` previously had a writer with no reader at all
    // (docs/frontend/API_GAPS.md item 4).
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_cost_summary_distinguishes_priced_from_unpriced_attempts: AUDIT_DATABASE_URL not set");
        return;
    };

    // Unique per test run so this test is safe to re-run against a
    // long-lived database (e.g. manual debugging) without a prior run's
    // rows under the same provider name polluting this run's filtered
    // queries.
    let run_id = Uuid::now_v7();
    let priced_provider = format!("priced-provider-{run_id}");
    let priced_model = format!("priced-model-{run_id}");
    let unpriced_provider = format!("unpriced-provider-{run_id}");
    let unpriced_model = format!("unpriced-model-{run_id}");

    let mut pricing = HashMap::new();
    pricing.insert(
        format!("{priced_provider}/{priced_model}"),
        PricingEntry {
            input_cost_per_1k: 1.0,
            output_cost_per_1k: 2.0,
        },
    );
    let writer = TelemetryWriter::new_with_fallback_log(&url, 2, 100, 60_000, true, true, "telemetry_fallback.jsonl", pricing)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let priced_request = Uuid::now_v7();
    let unpriced_request = Uuid::now_v7();
    let events = vec![
        // Priced: a configured provider/model pair.
        TelemetryEvent::RequestReceived {
            request_id: priced_request,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: priced_model.clone(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id: priced_request,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: priced_provider.clone(),
            model: priced_model.clone(),
        },
        TelemetryEvent::UsageRecorded {
            request_id: priced_request,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: 1000,
                completion_tokens: 500,
                total_tokens: 1500,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
        // Unpriced: no configured pricing entry for this provider/model.
        TelemetryEvent::RequestReceived {
            request_id: unpriced_request,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: unpriced_model.clone(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id: unpriced_request,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: unpriced_provider.clone(),
            model: unpriced_model.clone(),
        },
        TelemetryEvent::UsageRecorded {
            request_id: unpriced_request,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    // Scoped by provider so this test's assertions are exact regardless of
    // whatever other rows exist in a database shared across the test
    // binary's other tests.
    let (status, body) = get(state.clone(), &format!("/v1/analytics/costs?provider={priced_provider}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["currency"], "USD");
    assert_eq!(body["billed_cost"], serde_json::Value::Null);
    assert_eq!(body["eligible_attempts"], 1);
    assert_eq!(body["priced_attempts"], 1);
    assert_eq!(body["unpriced_attempts"], 0);
    // 1000 prompt tokens @ $1.00/1k + 500 completion tokens @ $2.00/1k = $2.00.
    let charge = body["estimated_token_charge"].as_f64().unwrap();
    assert!((charge - 2.0).abs() < 1e-9, "got {charge}");

    let breakdown = body["breakdowns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["provider"] == priced_provider.as_str())
        .unwrap_or_else(|| panic!("{priced_provider} breakdown not found: {body}"));
    assert_eq!(breakdown["priced_attempts"], 1);

    let (status, body) = get(state, &format!("/v1/analytics/costs?provider={unpriced_provider}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["eligible_attempts"], 1);
    assert_eq!(body["priced_attempts"], 0);
    assert_eq!(body["unpriced_attempts"], 1);
    assert_eq!(body["estimated_token_charge"], serde_json::Value::Null);
}

#[tokio::test]
async fn analytics_summary_covers_boundaries_retries_pending_and_usage_dedup() {
    // Exercises every acceptance criterion from docs/frontend/API_GAPS.md
    // item 1 in one seeded scenario: a boundary timestamp at each end of
    // the half-open [from, to) window, a multi-attempt retry chain, a
    // pending request (no RequestCompleted at all), a failed request with
    // no usage ever recorded, and a request with two cumulative usage
    // updates that must not be double-counted.
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_summary_covers_boundaries_retries_pending_and_usage_dedup: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    // A fixed, unique-per-run model name scopes this test's assertions
    // exactly, regardless of whatever else exists in a database shared
    // across the test binary's other tests.
    let model = format!("summary-test-model-{}", Uuid::now_v7());
    let t0 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(); // bucket 0 start — the `from` boundary, inclusive
    let t1 = t0 + chrono::Duration::hours(1); // bucket 1 start
    let t2 = t0 + chrono::Duration::hours(2); // the `to` boundary, exclusive

    fn received(request_id: Uuid, timestamp: chrono::DateTime<Utc>, model: &str) -> TelemetryEvent {
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp,
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: model.to_string(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        }
    }
    fn attempt_started(request_id: Uuid, n: u32, timestamp: chrono::DateTime<Utc>, provider: &str, model: &str) -> TelemetryEvent {
        TelemetryEvent::ProviderAttemptStarted { request_id, attempt_number: n, timestamp, provider: provider.to_string(), model: model.to_string() }
    }
    fn attempt_completed(request_id: Uuid, n: u32, timestamp: chrono::DateTime<Utc>, success: bool) -> TelemetryEvent {
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: n,
            timestamp,
            status: if success { ai_gateway::telemetry::events::AttemptStatus::Success } else { ai_gateway::telemetry::events::AttemptStatus::Failed },
            finish_reason: success.then(|| "stop".to_string()),
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 1000,
            error_code: (!success).then(|| "server_error".to_string()),
            error_message: (!success).then(|| "boom".to_string()),
        }
    }
    fn usage(request_id: Uuid, n: u32, timestamp: chrono::DateTime<Utc>, prompt: u64, completion: u64) -> TelemetryEvent {
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number: n,
            timestamp,
            usage: Usage { prompt_tokens: prompt, completion_tokens: completion, total_tokens: prompt + completion, cached_tokens: None, reasoning_tokens: None, raw_provider_usage: None },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        }
    }
    fn completed(request_id: Uuid, timestamp: chrono::DateTime<Utc>, attempts: u32, success: bool) -> TelemetryEvent {
        TelemetryEvent::RequestCompleted {
            request_id,
            timestamp,
            total_latency_us: 1000,
            total_attempts: attempts,
            serving_provider: Some("summary-test-provider".to_string()),
            resolved_model: None,
            success,
        }
    }

    let req_a = Uuid::now_v7(); // bucket 0, at the `from` boundary — included. Retry: attempt 1 fails, attempt 2 succeeds.
    let req_b = Uuid::now_v7(); // bucket 0 — pending (no RequestCompleted ever arrives)
    let req_c = Uuid::now_v7(); // bucket 1 — failed, no usage ever recorded
    let req_d = Uuid::now_v7(); // bucket 1 — succeeds, two cumulative usage updates on one attempt
    let req_e = Uuid::now_v7(); // exactly at the `to` boundary — must be excluded (half-open)

    let events = vec![
        // A: retry chain, then usage against the successful attempt.
        received(req_a, t0, &model),
        attempt_started(req_a, 1, t0, "summary-test-provider", &model),
        attempt_completed(req_a, 1, t0, false),
        attempt_started(req_a, 2, t0, "summary-test-provider", &model),
        usage(req_a, 2, t0, 100, 50),
        attempt_completed(req_a, 2, t0, true),
        completed(req_a, t0, 2, true),
        // B: received, then nothing else — genuinely pending.
        received(req_b, t0 + chrono::Duration::minutes(30), &model),
        // C: fails, no usage ever recorded for it.
        received(req_c, t1, &model),
        attempt_started(req_c, 1, t1, "summary-test-provider", &model),
        attempt_completed(req_c, 1, t1, false),
        completed(req_c, t1, 1, false),
        // D: succeeds, two cumulative usage updates — canonical (most
        // recent) must be the one counted, not both summed.
        received(req_d, t1 + chrono::Duration::minutes(30), &model),
        attempt_started(req_d, 1, t1, "summary-test-provider", &model),
        usage(req_d, 1, t1, 200, 5),
        usage(req_d, 1, t1, 200, 20),
        attempt_completed(req_d, 1, t1, true),
        completed(req_d, t1, 1, true),
        // E: exactly at `to` — must not appear anywhere in the response.
        received(req_e, t2, &model),
        attempt_started(req_e, 1, t2, "summary-test-provider", &model),
        usage(req_e, 1, t2, 999, 999),
        attempt_completed(req_e, 1, t2, true),
        completed(req_e, t2, 1, true),
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    // `+00:00` (rfc3339's default UTC suffix) contains a literal `+`,
    // which a URL-encoded query string decodes as a space — use the `Z`
    // suffix form instead so this doesn't corrupt the timestamp.
    let from_str = t0.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let to_str = t2.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (status, body) = get(
        state,
        &format!("/v1/analytics/summary?from={from_str}&to={to_str}&bucket=hour&model={model}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "response: {body}");

    // Whole-window totals: E (at `to`) must be excluded entirely.
    assert_eq!(body["requests"]["total"], 4, "{body}");
    assert_eq!(body["requests"]["successful"], 2, "{body}"); // A, D
    assert_eq!(body["requests"]["failed"], 1, "{body}"); // C
    assert_eq!(body["requests"]["pending"], 1, "{body}"); // B
    assert_eq!(body["attempts"]["total"], 4, "{body}"); // A(2) + C(1) + D(1)
    assert_eq!(body["attempts"]["failed"], 2, "{body}"); // A's attempt 1, C's attempt 1

    // Usage: A contributes 100/50, D contributes its *last* update
    // (200/20), not the sum of both of its updates (200/5 + 200/20).
    assert_eq!(body["usage"]["prompt"], 300, "{body}");
    assert_eq!(body["usage"]["completion"], 70, "{body}");
    assert_eq!(body["usage"]["known_requests"], 2, "{body}"); // A, D — not C
    assert_eq!(body["usage"]["eligible_requests"], 4, "{body}");

    // Series: exactly two hourly buckets, correctly split.
    let series = body["series"].as_array().unwrap();
    assert_eq!(series.len(), 2, "{body}");

    let bucket0 = &series[0];
    let bucket0_start: chrono::DateTime<Utc> = bucket0["bucket_start"].as_str().unwrap().parse().unwrap();
    assert_eq!(bucket0_start, t0);
    assert_eq!(bucket0["requests_total"], 2, "{body}"); // A, B
    assert_eq!(bucket0["requests_successful"], 1, "{body}");
    assert_eq!(bucket0["requests_pending"], 1, "{body}");
    assert_eq!(bucket0["attempts_total"], 2, "{body}"); // A's two attempts
    assert_eq!(bucket0["usage_prompt"], 100, "{body}");
    assert_eq!(bucket0["usage_known_requests"], 1, "{body}");

    let bucket1 = &series[1];
    assert_eq!(bucket1["requests_total"], 2, "{body}"); // C, D
    assert_eq!(bucket1["requests_failed"], 1, "{body}");
    assert_eq!(bucket1["requests_successful"], 1, "{body}");
    // C has no usage at all — must be null, not 0, once D's own usage
    // (200 prompt) is the only contribution in this bucket.
    assert_eq!(bucket1["usage_prompt"], 200, "{body}");
    assert_eq!(bucket1["usage_completion"], 20, "{body}");
    assert_eq!(bucket1["usage_known_requests"], 1, "{body}"); // D only
}

#[tokio::test]
async fn analytics_summary_rejects_invalid_bucket_and_oversized_range() {
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_summary_rejects_invalid_bucket_and_oversized_range: AUDIT_DATABASE_URL not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, _) = get(state.clone(), "/v1/analytics/summary?bucket=fortnight").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 1000 days at minute granularity vastly exceeds the 500-bucket cap.
    let from = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    let to = from + chrono::Duration::days(1000);
    let (status, _) = get(
        state,
        &format!(
            "/v1/analytics/summary?from={}&to={}&bucket=minute",
            from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn analytics_requests_keyset_pagination_is_stable_under_concurrent_inserts() {
    // The previous `received_at DESC` + `LIMIT/OFFSET` pagination could
    // duplicate or omit rows if a new request was inserted between two
    // page fetches, because OFFSET counts positions that shift under
    // concurrent inserts. Keyset pagination continues strictly from the
    // last row's own `(received_at, request_id)`, which is stable
    // regardless of what gets inserted afterward (docs/frontend/API_GAPS.md
    // item 6).
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_requests_keyset_pagination_is_stable_under_concurrent_inserts: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let model = format!("keyset-test-model-{}", Uuid::now_v7());
    let t = Utc.with_ymd_and_hms(2021, 6, 1, 12, 0, 0).unwrap();

    fn received(request_id: Uuid, timestamp: chrono::DateTime<Utc>, model: &str) -> TelemetryEvent {
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp,
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: model.to_string(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        }
    }

    // Two requests share the exact same `received_at` (a real tie the
    // `request_id` tie-break must resolve deterministically), two more are
    // strictly older.
    let r1 = Uuid::now_v7();
    let r2 = Uuid::now_v7();
    let r3 = Uuid::now_v7();
    let r4 = Uuid::now_v7();
    let seed_events = vec![
        received(r1, t, &model),
        received(r2, t, &model),
        received(r3, t - chrono::Duration::seconds(1), &model),
        received(r4, t - chrono::Duration::seconds(2), &model),
    ];
    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in seed_events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool.clone()),
    });

    let (status, page1) = get(state.clone(), &format!("/v1/analytics/requests?model={model}&limit=2")).await;
    assert_eq!(status, StatusCode::OK, "{page1}");
    let page1_ids: Vec<String> = page1["requests"].as_array().unwrap().iter().map(|r| r["request_id"].as_str().unwrap().to_string()).collect();
    assert_eq!(page1_ids.len(), 2, "{page1}");
    let next_cursor = page1["next_cursor"].as_str().unwrap_or_default().to_string();
    assert!(!next_cursor.is_empty(), "expected a next_cursor with 4 rows and limit=2: {page1}");

    // Simulate a concurrent insert: a brand-new request, newer than
    // everything already fetched, arriving *between* the two page fetches.
    let writer2 = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let r5 = Uuid::now_v7();
    let (tx2, rx2) = tokio::sync::mpsc::channel(10);
    tx2.send(received(r5, t + chrono::Duration::seconds(100), &model)).await.unwrap();
    drop(tx2);
    writer2.run(rx2).await;

    let (status, page2) = get(
        state,
        &format!("/v1/analytics/requests?model={model}&limit=2&cursor={next_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page2}");
    let page2_ids: Vec<String> = page2["requests"].as_array().unwrap().iter().map(|r| r["request_id"].as_str().unwrap().to_string()).collect();

    // r5 (inserted after page1 was fetched, newer than the cursor) must
    // never appear in a continuation of that same traversal. r3/r4 (the
    // two rows strictly after the cursor) must appear exactly once between
    // the two pages, with no duplication of r1/r2.
    let mut all_ids: Vec<String> = page1_ids.iter().cloned().chain(page2_ids.iter().cloned()).collect();
    all_ids.sort();
    let mut expected: Vec<String> = vec![r1, r2, r3, r4].into_iter().map(|id| id.to_string()).collect();
    expected.sort();
    assert_eq!(all_ids, expected, "page1: {page1}\npage2: {page2}");
    assert!(!page2["requests"].as_array().unwrap().iter().any(|r| r["request_id"] == r5.to_string()), "r5 leaked into the continuation: {page2}");
}

#[tokio::test]
async fn analytics_requests_supports_attempted_provider_application_task_and_tag_filters() {
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_requests_supports_attempted_provider_application_task_and_tag_filters: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let model = format!("filter-test-model-{}", Uuid::now_v7());
    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: Some("filter-test-app".to_string()),
            task: Some("filter-test-task".to_string()),
            tags: vec!["filter-test-tag".to_string()],
            requested_model: model.clone(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        // Fails on the first provider, then succeeds on a second — so
        // `attempted_provider` (any attempt) and `provider` (final serving
        // provider) must disagree for the first one.
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: "filter-test-attempted-only".to_string(),
            model: model.clone(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Failed,
            finish_reason: None,
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 100,
            error_code: Some("server_error".to_string()),
            error_message: Some("boom".to_string()),
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 2,
            timestamp: Utc::now(),
            provider: "filter-test-serving".to_string(),
            model: model.clone(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 2,
            timestamp: Utc::now(),
            status: ai_gateway::telemetry::events::AttemptStatus::Success,
            finish_reason: Some("stop".to_string()),
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 100,
            error_code: None,
            error_message: None,
        },
        TelemetryEvent::RequestCompleted {
            request_id,
            timestamp: Utc::now(),
            total_latency_us: 200,
            total_attempts: 2,
            serving_provider: Some("filter-test-serving".to_string()),
            resolved_model: Some(model.clone()),
            success: true,
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let found = |body: &serde_json::Value| -> bool {
        body["requests"].as_array().unwrap().iter().any(|r| r["request_id"] == request_id.to_string())
    };

    // `attempted_provider` must find it via the failed first attempt, even
    // though that provider never served the request.
    let (status, body) = get(state.clone(), "/v1/analytics/requests?attempted_provider=filter-test-attempted-only").await;
    assert_eq!(status, StatusCode::OK);
    assert!(found(&body), "{body}");

    // `provider` (serving_provider) must NOT match the attempted-but-failed
    // provider — that's exactly the distinction item 6 asked for.
    let (status, body) = get(state.clone(), "/v1/analytics/requests?provider=filter-test-attempted-only").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!found(&body), "attempted-only provider must not match the serving_provider filter: {body}");

    let (status, body) = get(state.clone(), "/v1/analytics/requests?provider=filter-test-serving").await;
    assert_eq!(status, StatusCode::OK);
    assert!(found(&body), "{body}");

    let (status, body) = get(state.clone(), "/v1/analytics/requests?application=filter-test-app").await;
    assert_eq!(status, StatusCode::OK);
    assert!(found(&body), "{body}");

    let (status, body) = get(state.clone(), "/v1/analytics/requests?task=filter-test-task").await;
    assert_eq!(status, StatusCode::OK);
    assert!(found(&body), "{body}");

    let (status, body) = get(state, "/v1/analytics/requests?tag=filter-test-tag").await;
    assert_eq!(status, StatusCode::OK);
    assert!(found(&body), "{body}");
}

#[tokio::test]
async fn analytics_request_detail_includes_attempt_spans_routing_errors_and_costs() {
    // docs/frontend/API_GAPS.md item 5 (excluding the choice-ID piece,
    // which needs a write-side change, not just a new query): attempt
    // start/finish spans, and previously entirely-omitted routing/errors/
    // costs sections in request detail.
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_request_detail_includes_attempt_spans_routing_errors_and_costs: AUDIT_DATABASE_URL not set");
        return;
    };

    let mut pricing = HashMap::new();
    pricing.insert(
        "detail-test-serving/detail-test-model".to_string(),
        PricingEntry { input_cost_per_1k: 1.0, output_cost_per_1k: 2.0 },
    );
    let writer = TelemetryWriter::new_with_fallback_log(&url, 2, 100, 60_000, true, true, "telemetry_fallback.jsonl", pricing)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let started_at = Utc::now();
    let finished_at = started_at + chrono::Duration::milliseconds(250);

    let routing_decision = ai_gateway::routing::RoutingDecision {
        timestamp: Utc::now(),
        requested_model: "detail-test-alias".to_string(),
        resolution_type: "alias".to_string(),
        candidates_considered: 2,
        candidates_rejected: 1,
        rejection_reasons: vec![ai_gateway::routing::RejectionRecord {
            provider: "detail-test-rejected".to_string(),
            model: "detail-test-model".to_string(),
            reason: "provider_unavailable".to_string(),
        }],
        selected_provider: "detail-test-serving".to_string(),
        selected_model: "detail-test-model".to_string(),
        policy_applied: "ordered".to_string(),
        short_circuit: None,
    };

    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: started_at,
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: "detail-test-alias".to_string(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::RouteSelected { request_id, timestamp: Utc::now(), decision: routing_decision },
        // Attempt 1 fails on a different provider, producing an `errors` row.
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: started_at,
            provider: "detail-test-failed".to_string(),
            model: "detail-test-model".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 1,
            timestamp: started_at + chrono::Duration::milliseconds(50),
            status: ai_gateway::telemetry::events::AttemptStatus::Failed,
            finish_reason: None,
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 50_000,
            error_code: Some("server_error".to_string()),
            error_message: Some("upstream 503".to_string()),
        },
        // Attempt 2 succeeds, with usage feeding a priced cost record.
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 2,
            timestamp: started_at,
            provider: "detail-test-serving".to_string(),
            model: "detail-test-model".to_string(),
        },
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number: 2,
            timestamp: finished_at,
            usage: Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500, cached_tokens: None, reasoning_tokens: None, raw_provider_usage: None },
            source: "non_streaming".to_string(),
            kind: "cumulative".to_string(),
        },
        TelemetryEvent::ProviderAttemptCompleted {
            request_id,
            attempt_number: 2,
            timestamp: finished_at,
            status: ai_gateway::telemetry::events::AttemptStatus::Success,
            finish_reason: Some("stop".to_string()),
            provider_response_id: None,
            upstream_provider: None,
            duration_us: 200_000,
            error_code: None,
            error_message: None,
        },
        TelemetryEvent::RequestCompleted {
            request_id,
            timestamp: finished_at,
            total_latency_us: 250_000,
            total_attempts: 2,
            serving_provider: Some("detail-test-serving".to_string()),
            resolved_model: Some("detail-test-model".to_string()),
            success: true,
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, &format!("/v1/analytics/requests/{request_id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let attempts = body["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{body}");
    let attempt2 = attempts.iter().find(|a| a["attempt_number"] == 2).unwrap();
    assert!(attempt2["started_at"].is_string(), "{body}");
    assert!(attempt2["finished_at"].is_string(), "{body}");
    assert!(attempt2["duration_definition"].as_str().unwrap().contains("dispatch"), "{body}");
    let started: chrono::DateTime<Utc> = attempt2["started_at"].as_str().unwrap().parse().unwrap();
    let finished: chrono::DateTime<Utc> = attempt2["finished_at"].as_str().unwrap().parse().unwrap();
    assert!(finished > started, "{body}");

    let routing = body["routing"].as_array().unwrap();
    assert_eq!(routing.len(), 1, "{body}");
    assert_eq!(routing[0]["selected_provider"], "detail-test-serving");
    assert_eq!(routing[0]["candidates_rejected"], 1);
    assert_eq!(routing[0]["rejection_reasons"][0]["provider"], "detail-test-rejected");

    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{body}");
    assert_eq!(errors[0]["error_code"], "server_error");
    assert_eq!(errors[0]["provider"], serde_json::Value::Null); // `errors` table doesn't carry provider from this call site — see AttemptRow/errors for the authoritative provider instead.

    let costs = body["costs"].as_array().unwrap();
    assert_eq!(costs.len(), 1, "{body}");
    assert_eq!(costs[0]["provider"], "detail-test-serving");
    let estimated = costs[0]["estimated_cost"].as_f64().unwrap();
    assert!((estimated - 2.0).abs() < 1e-9, "{body}");
}

#[tokio::test]
async fn analytics_timeline_cursor_pagination_returns_each_event_exactly_once() {
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_timeline_cursor_pagination_returns_each_event_exactly_once: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let mut events = vec![TelemetryEvent::RequestReceived {
        request_id,
        timestamp: Utc::now(),
        client_correlation_id: None,
        application: None,
        task: None,
        tags: vec![],
        requested_model: "timeline-cursor-model".to_string(),
        streaming: true,
        canonical_messages: None,
        raw_request: None,
        uses_tools: false,
        uses_vision: false,
        uses_structured_output: false,
        uses_reasoning: false,
    }];
    // Five stream chunks, sequenced 1..=5.
    for seq in 1..=5u64 {
        events.push(TelemetryEvent::StreamChunk {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            sequence: seq,
            chunk_type: "content".to_string(),
            byte_count: 4,
            synthetic: false,
        });
    }

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let mut all_sequences: Vec<i64> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(pages <= 10, "pagination did not terminate");
        let path = match &cursor {
            Some(c) => format!("/v1/analytics/requests/{request_id}/timeline?limit=2&after={c}"),
            None => format!("/v1/analytics/requests/{request_id}/timeline?limit=2"),
        };
        let (status, body) = get(state.clone(), &path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let events = body["events"].as_array().unwrap();
        for e in events {
            all_sequences.push(e["sequence"].as_i64().unwrap());
        }
        let complete = body["complete"].as_bool().unwrap();
        match body["next_cursor"].as_str() {
            Some(next) => {
                assert!(!complete, "next_cursor present but complete=true: {body}");
                cursor = Some(next.to_string());
            }
            None => {
                assert!(complete, "no next_cursor but complete=false: {body}");
                break;
            }
        }
    }

    assert_eq!(pages, 3, "expected 3 pages of 2+2+1 for 5 events");
    assert_eq!(all_sequences, vec![1, 2, 3, 4, 5], "each sequence must appear exactly once, in order");
}

#[tokio::test]
async fn analytics_request_detail_exposes_stable_id_and_choice_index_per_generation() {
    // docs/frontend/API_GAPS.md item 5's choice-ID gap: `n > 1` must
    // produce separately readable generation rows, each with its own
    // stable `id` and `choice_index` — not one row merging every choice.
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_request_detail_exposes_stable_id_and_choice_index_per_generation: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: "choice-index-test-model".to_string(),
            streaming: false,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        TelemetryEvent::ProviderAttemptStarted {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            provider: "choice-index-test-provider".to_string(),
            model: "choice-index-test-model".to_string(),
        },
        TelemetryEvent::GenerationContent {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            choice_index: 0,
            content: "first choice".to_string(),
            reasoning_content: Some("because reasons".to_string()),
            tool_calls: None,
            raw_response: Some(serde_json::json!({"raw": "body"})),
            status: "complete".to_string(),
        },
        TelemetryEvent::GenerationContent {
            request_id,
            attempt_number: 1,
            timestamp: Utc::now(),
            choice_index: 1,
            content: "second choice".to_string(),
            reasoning_content: None,
            tool_calls: None,
            raw_response: None,
            status: "complete".to_string(),
        },
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, &format!("/v1/analytics/requests/{request_id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let generations = body["generations"].as_array().unwrap();
    assert_eq!(generations.len(), 2, "{body}");

    let choice0 = generations.iter().find(|g| g["choice_index"] == 0).unwrap();
    let choice1 = generations.iter().find(|g| g["choice_index"] == 1).unwrap();

    assert_eq!(choice0["content"], "first choice");
    assert_eq!(choice0["reasoning_content"], "because reasons");
    assert_eq!(choice0["status"], "complete");
    assert!(choice0["id"].is_i64() || choice0["id"].is_u64(), "{body}");

    assert_eq!(choice1["content"], "second choice");
    assert_eq!(choice1["reasoning_content"], serde_json::Value::Null);
    assert_eq!(choice1["status"], "complete");

    // Stable IDs must actually be distinct per generation row.
    assert_ne!(choice0["id"], choice1["id"], "{body}");
}

#[tokio::test]
async fn analytics_usage_canonical_flags_single_duplicate_and_conflicting_observations() {
    // docs/frontend/API_GAPS.md item 2: every provider this gateway
    // supports reports usage once per attempt as a cumulative total (see
    // `TelemetryEvent::UsageRecorded`'s doc comment), so the normal case is
    // exactly one observation per attempt. More than one is an anomaly,
    // not a designed delta stream — this must be detected and reported,
    // not silently resolved.
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping analytics_usage_canonical_flags_single_duplicate_and_conflicting_observations: AUDIT_DATABASE_URL not set");
        return;
    };

    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    fn usage_event(request_id: Uuid, attempt_number: u32, prompt: u64, completion: u64) -> TelemetryEvent {
        TelemetryEvent::UsageRecorded {
            request_id,
            attempt_number,
            timestamp: Utc::now(),
            usage: Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                cached_tokens: None,
                reasoning_tokens: None,
                raw_provider_usage: None,
            },
            source: "stream_final".to_string(),
            kind: "cumulative".to_string(),
        }
    }

    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: "usage-quality-test-model".to_string(),
            streaming: true,
            canonical_messages: None,
            raw_request: None,
            uses_tools: false,
            uses_vision: false,
            uses_structured_output: false,
            uses_reasoning: false,
        },
        // Attempt 1: exactly one observation — the normal case.
        usage_event(request_id, 1, 100, 50),
        // Attempt 2: two identical observations — a redelivered event, not
        // a real anomaly, but still worth flagging as more than one.
        usage_event(request_id, 2, 200, 80),
        usage_event(request_id, 2, 200, 80),
        // Attempt 3: two genuinely different observations — a real
        // anomaly. Canonical must be the most recent (300/120), and the
        // flag must say so plainly rather than silently averaging or
        // summing them.
        usage_event(request_id, 3, 300, 100),
        usage_event(request_id, 3, 300, 120),
    ];

    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;

    let config = GatewayConfig::load(None).unwrap();
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::new()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
        llama_server: None,
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, &format!("/v1/analytics/requests/{request_id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Raw observations are all preserved, untouched.
    let usage = body["usage"].as_array().unwrap();
    assert_eq!(usage.len(), 5, "{body}");
    assert_eq!(usage[0]["source"], "stream_final");
    assert_eq!(usage[0]["kind"], "cumulative");

    let canonical = body["usage_canonical"].as_array().unwrap();
    let by_attempt = |n: i64| canonical.iter().find(|c| c["attempt_number"] == n).unwrap();

    let a1 = by_attempt(1);
    assert_eq!(a1["quality"], "single_observation");
    assert_eq!(a1["observation_count"], 1);
    assert_eq!(a1["prompt_tokens"], 100);

    let a2 = by_attempt(2);
    assert_eq!(a2["quality"], "duplicate_observations");
    assert_eq!(a2["observation_count"], 2);
    assert_eq!(a2["prompt_tokens"], 200);

    let a3 = by_attempt(3);
    assert_eq!(a3["quality"], "conflicting_observations");
    assert_eq!(a3["observation_count"], 2);
    // Canonical is the most recently observed value, not a sum or average.
    assert_eq!(a3["prompt_tokens"], 300);
    assert_eq!(a3["completion_tokens"], 120);
}
