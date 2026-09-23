//! Verifies telemetry-writer behavior that can only be checked against a
//! real Postgres: reasoning-visibility ordering (Phase 3.2), model/provider
//! snapshot persistence (Phase 3.4), and cost-record computation (Phase
//! 3.5). These were previously "verified by code review only" per
//! docs/REMEDIATION_PLAN.md, since this environment has no database by
//! default. Each test degrades to a no-op when `AUDIT_DATABASE_URL` isn't
//! set, same pattern as `tests/audit_contract_tests.rs`'s A02 probe:
//!
//!   docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16
//!   AUDIT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/postgres \
//!     cargo test --test db_verification_tests

use std::collections::HashMap;

use ai_gateway::config::PricingEntry;
use ai_gateway::telemetry::{TelemetryEvent, TelemetryWriter};
use ai_gateway::types::Usage;
use chrono::Utc;
use uuid::Uuid;

/// Returns `None` (causing the caller to skip) unless `AUDIT_DATABASE_URL`
/// is set, and otherwise resets the target database to a clean slate so
/// each test starts from a known state.
async fn writer_and_pool() -> Option<(TelemetryWriter, sqlx::PgPool)> {
    let url = std::env::var("AUDIT_DATABASE_URL").ok()?;
    let writer = TelemetryWriter::new(&url, 2, 100, 60_000, true, true)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    Some((writer, pool))
}

async fn run_events(writer: TelemetryWriter, events: Vec<TelemetryEvent>) {
    let (tx, rx) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(rx).await;
}

fn request_received(request_id: Uuid, model: &str) -> TelemetryEvent {
    TelemetryEvent::RequestReceived {
        request_id,
        timestamp: Utc::now(),
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

fn attempt_started(request_id: Uuid, provider: &str, model: &str) -> TelemetryEvent {
    TelemetryEvent::ProviderAttemptStarted {
        request_id,
        attempt_number: 1,
        timestamp: Utc::now(),
        provider: provider.to_string(),
        model: model.to_string(),
    }
}

#[tokio::test]
async fn reasoning_visibility_is_correct_regardless_of_event_order() {
    let Some((writer, pool)) = writer_and_pool().await else {
        eprintln!("skipping reasoning_visibility_is_correct_regardless_of_event_order: AUDIT_DATABASE_URL not set");
        return;
    };

    let content_then_usage = Uuid::now_v7();
    let usage_then_content = Uuid::now_v7();
    let exposed = Uuid::now_v7();
    let plain_none = Uuid::now_v7();

    let usage_with_reasoning = || Usage {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        cached_tokens: None,
        reasoning_tokens: Some(5),
        raw_provider_usage: None,
    };

    let events = vec![
        // Scenario 1: GenerationContent (no text) arrives before the
        // UsageRecorded that reveals reasoning happened — must be patched
        // from 'none' to 'count_only' after the fact.
        request_received(content_then_usage, "m"),
        attempt_started(content_then_usage, "p", "m"),
        TelemetryEvent::GenerationContent {
            request_id: content_then_usage,
            attempt_number: 1,
            timestamp: Utc::now(),
            content: "answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            raw_response: None,
        },
        TelemetryEvent::UsageRecorded {
            request_id: content_then_usage,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: usage_with_reasoning(),
        },
        // Scenario 2: UsageRecorded arrives first — GenerationContent must
        // look it up and write 'count_only' directly.
        request_received(usage_then_content, "m"),
        attempt_started(usage_then_content, "p", "m"),
        TelemetryEvent::UsageRecorded {
            request_id: usage_then_content,
            attempt_number: 1,
            timestamp: Utc::now(),
            usage: usage_with_reasoning(),
        },
        TelemetryEvent::GenerationContent {
            request_id: usage_then_content,
            attempt_number: 1,
            timestamp: Utc::now(),
            content: "answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            raw_response: None,
        },
        // Scenario 3: reasoning text was actually exposed.
        request_received(exposed, "m"),
        attempt_started(exposed, "p", "m"),
        TelemetryEvent::GenerationContent {
            request_id: exposed,
            attempt_number: 1,
            timestamp: Utc::now(),
            content: "answer".to_string(),
            reasoning_content: Some("because...".to_string()),
            tool_calls: None,
            raw_response: None,
        },
        // Scenario 4: no reasoning at all.
        request_received(plain_none, "m"),
        attempt_started(plain_none, "p", "m"),
        TelemetryEvent::GenerationContent {
            request_id: plain_none,
            attempt_number: 1,
            timestamp: Utc::now(),
            content: "answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            raw_response: None,
        },
    ];

    run_events(writer, events).await;

    let fetch_visibility = |id: Uuid, pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT reasoning_visibility FROM generations WHERE request_id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };

    assert_eq!(fetch_visibility(content_then_usage, &pool).await, "count_only");
    assert_eq!(fetch_visibility(usage_then_content, &pool).await, "count_only");
    assert_eq!(fetch_visibility(exposed, &pool).await, "exposed");
    assert_eq!(fetch_visibility(plain_none, &pool).await, "none");
}

#[tokio::test]
async fn model_and_provider_snapshots_are_persisted() {
    let Some((writer, pool)) = writer_and_pool().await else {
        eprintln!("skipping model_and_provider_snapshots_are_persisted: AUDIT_DATABASE_URL not set");
        return;
    };

    // Both tables existed in the schema with no writer at all (Audit
    // A15/CL08) before docs/REMEDIATION_PLAN.md Phase 3.4.
    let model_name = format!("audit-model-{}", Uuid::now_v7());
    let provider_name = format!("audit-provider-{}", Uuid::now_v7());

    let events = vec![
        TelemetryEvent::ModelSnapshot {
            timestamp: Utc::now(),
            model_name: model_name.clone(),
            providers: vec!["p1".to_string(), "p2".to_string()],
            capabilities: serde_json::json!({ "vision": false, "tools": true }),
        },
        TelemetryEvent::ProviderSnapshot {
            timestamp: Utc::now(),
            provider_name: provider_name.clone(),
            provider_type: "open_ai_compatible".to_string(),
            health: "healthy".to_string(),
            total_requests: 42,
            total_errors: 1,
            avg_latency_ms: Some(123.4),
        },
    ];

    run_events(writer, events).await;

    let model_count: i64 = sqlx::query_scalar("SELECT count(*) FROM model_snapshots WHERE model_name = $1")
        .bind(&model_name)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(model_count, 1);

    let provider_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM provider_snapshots WHERE provider_name = $1")
            .bind(&provider_name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(provider_count, 1);
}

#[tokio::test]
async fn cost_record_is_computed_when_pricing_is_configured() {
    let url = match std::env::var("AUDIT_DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping cost_record_is_computed_when_pricing_is_configured: AUDIT_DATABASE_URL not set");
            return;
        }
    };

    let mut pricing = HashMap::new();
    pricing.insert(
        "cost-test-provider/cost-test-model".to_string(),
        PricingEntry {
            input_cost_per_1k: 1.0,
            output_cost_per_1k: 2.0,
        },
    );
    let writer = TelemetryWriter::new_with_fallback_log(
        &url,
        2,
        100,
        60_000,
        true,
        true,
        "telemetry_fallback.jsonl",
        pricing,
    )
    .await
    .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let request_id = Uuid::now_v7();
    let events = vec![
        request_received(request_id, "m"),
        attempt_started(request_id, "cost-test-provider", "cost-test-model"),
        TelemetryEvent::UsageRecorded {
            request_id,
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
    ];

    run_events(writer, events).await;

    // 1000 prompt tokens @ $1.00/1k = $1.00; 500 completion tokens @
    // $2.00/1k = $1.00; total $2.00. `cost_records` previously had no
    // producer at all regardless of configuration (Audit A15/A17).
    let (estimated_cost, input_cost, output_cost): (f64, f64, f64) = sqlx::query_as(
        "SELECT estimated_cost, input_cost, output_cost FROM cost_records WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!((estimated_cost - 2.0).abs() < 1e-9, "got {estimated_cost}");
    assert!((input_cost - 1.0).abs() < 1e-9, "got {input_cost}");
    assert!((output_cost - 1.0).abs() < 1e-9, "got {output_cost}");
}
