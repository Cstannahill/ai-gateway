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
use chrono::Utc;
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

    let request_id = Uuid::now_v7();
    let events = vec![
        TelemetryEvent::RequestReceived {
            request_id,
            timestamp: Utc::now(),
            client_correlation_id: None,
            application: None,
            task: None,
            tags: vec![],
            requested_model: "dup-usage-model".to_string(),
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
            provider: "dup-usage-provider".to_string(),
            model: "dup-usage-model".to_string(),
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
        analytics_pool: Some(pool),
    });

    let (status, body) = get(state, "/v1/analytics/providers/comparison").await;
    assert_eq!(status, StatusCode::OK);
    let row = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "dup-usage-provider")
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
