//! Audit probes for INIT_CONVERSATION.md. These assert the intended contract,
//! not the defective behavior. Ignored until remediated; run explicitly with:
//! cargo test --test audit_contract_tests --offline -- --ignored
use ai_gateway::{
    config::GatewayConfig,
    providers::{
        Provider, ProviderOutput, events::ProviderEvent, openai_compatible::OpenAICompatibleAdapter,
    },
    registry::{ModelRegistry, ProviderRegistry},
    routing::Router as RoutingEngine,
    server::{AppState, build_router},
    telemetry::TelemetryEvent,
    types::*,
};
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tower::ServiceExt;

/// A minimal embeddings-capable provider used to verify embeddings now
/// share attempt-level telemetry/retry with the other endpoints
/// (docs/REMEDIATION_PLAN.md Phase 1.1/2.4 — Audit A14).
struct EmbeddingProbeProvider {
    fail: bool,
}
#[async_trait]
impl Provider for EmbeddingProbeProvider {
    fn name(&self) -> &str {
        "embed"
    }
    fn capabilities(&self) -> &ModelCapabilities {
        static CAPS: std::sync::OnceLock<ModelCapabilities> = std::sync::OnceLock::new();
        CAPS.get_or_init(|| ModelCapabilities {
            embeddings: true,
            ..Default::default()
        })
    }
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Healthy
    }
    async fn execute(&self, _request: &GatewayRequest, _model: &str) -> anyhow::Result<ProviderOutput> {
        anyhow::bail!("not used")
    }
    async fn embeddings(&self, _model: &str, _input: &Value) -> anyhow::Result<Value> {
        if self.fail {
            anyhow::bail!("embedding backend unreachable");
        }
        Ok(json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2]}],"usage":{"prompt_tokens":3,"total_tokens":3}}))
    }
}

struct ProbeProvider {
    name: String,
    mode: u8,
    caps: ModelCapabilities,
    seen: Arc<Mutex<Vec<GatewayRequest>>>,
}
#[async_trait]
impl Provider for ProbeProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn capabilities(&self) -> &ModelCapabilities {
        &self.caps
    }
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Healthy
    }
    async fn execute(
        &self,
        request: &GatewayRequest,
        model: &str,
    ) -> anyhow::Result<ProviderOutput> {
        self.seen.lock().unwrap().push(request.clone());
        let error = ProviderEvent::Error {
            timestamp: Utc::now(),
            code: "server_error".into(),
            message: "audit failure".into(),
            status_code: Some(503),
            retryable: true,
        };
        if self.mode == 1 {
            return Ok(ProviderOutput::Complete(error));
        }
        let mut events = vec![
            ProviderEvent::Started {
                timestamp: Utc::now(),
                provider: self.name.clone(),
                model: model.into(),
            },
            ProviderEvent::ContentDelta {
                timestamp: Utc::now(),
                index: 0,
                delta: "partial output".into(),
            },
        ];
        if self.mode == 3 || self.mode == 4 {
            // Simulates an `n > 1` response: a second choice's content
            // delta at index 1, both wrapped up by one Completed event
            // (docs/REMEDIATION_PLAN.md Phase 2.3 / Audit A08).
            events.push(ProviderEvent::ContentDelta {
                timestamp: Utc::now(),
                index: 1,
                delta: "second choice output".into(),
            });
        }
        if self.mode == 2 || self.mode == 4 {
            // Mode 4: an `n > 1` stream that fails mid-stream — both
            // choices' partial content must be preserved, each with its
            // own GenerationContent event marked `status: "partial"`
            // (docs/frontend/API_GAPS.md item 5).
            events.push(error);
        } else {
            events.push(ProviderEvent::Completed {
                timestamp: Utc::now(),
                finish_reason: Some("stop".into()),
                provider_response_id: Some("upstream-id".into()),
                upstream_provider: None,
                raw_response: None,
            });
        }
        Ok(ProviderOutput::Stream(Box::pin(futures::stream::iter(
            events,
        ))))
    }
}
fn pc(name: &str, enabled: bool) -> ProviderConfig {
    serde_json::from_value(json!({"name":name,"type":"open_ai_compatible","base_url":"http://127.0.0.1:1","enabled":enabled})).unwrap()
}
fn setup(
    mode: u8,
    enabled: bool,
) -> (
    Arc<AppState>,
    tokio::sync::mpsc::Receiver<TelemetryEvent>,
    Arc<Mutex<Vec<GatewayRequest>>>,
) {
    let mut config: GatewayConfig = serde_yaml::from_str("{}").unwrap();
    config.routing.max_retries_per_provider = 1;
    config.routing.fallback_on.clear();
    let seen = Arc::new(Mutex::new(vec![]));
    let mut providers = ProviderRegistry::new();
    for (name, m) in [("first", mode), ("second", 0)] {
        providers.register(
            pc(name, enabled),
            Arc::new(ProbeProvider {
                name: name.into(),
                mode: m,
                caps: ModelCapabilities::default(),
                seen: seen.clone(),
            }),
        );
    }
    let models = serde_json::from_value(json!({"alias": {"name":"alias","candidates":[{"provider":"first","model":"m","priority":1},{"provider":"second","model":"m","priority":2}]}})).unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    (
        Arc::new(AppState {
            config,
            model_registry: ModelRegistry::new(models),
            provider_registry: providers,
            router: RoutingEngine::new("ordered".into()),
            telemetry_tx: tx,
            start_time: Instant::now(),
            total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
            telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
            analytics_pool: None,
        }),
        rx,
        seen,
    )
}
async fn post(state: Arc<AppState>, path: &str, body: Value) -> (u16, String) {
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}
fn drain(rx: &mut tokio::sync::mpsc::Receiver<TelemetryEvent>) -> Vec<TelemetryEvent> {
    let mut events = vec![];
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    events
}

fn setup_embeddings(
    fail: bool,
) -> (Arc<AppState>, tokio::sync::mpsc::Receiver<TelemetryEvent>) {
    let mut config: GatewayConfig = serde_yaml::from_str("{}").unwrap();
    config.routing.max_retries_per_provider = 1;
    let mut providers = ProviderRegistry::new();
    providers.register(pc("embed", true), Arc::new(EmbeddingProbeProvider { fail }));
    let models = serde_json::from_value(
        json!({"alias": {"name":"alias","candidates":[{"provider":"embed","model":"m","priority":1}]}}),
    )
    .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    (
        Arc::new(AppState {
            config,
            model_registry: ModelRegistry::new(models),
            provider_registry: providers,
            router: RoutingEngine::new("ordered".into()),
            telemetry_tx: tx,
            start_time: Instant::now(),
            total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
            telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(ai_gateway::telemetry::TelemetryMode::Disabled)),
        response_cache: None,
            analytics_pool: None,
        }),
        rx,
    )
}
#[tokio::test]
async fn embeddings_must_record_attempt_telemetry() {
    let (state, mut rx) = setup_embeddings(false);
    let (status, _) = post(
        state,
        "/v1/embeddings",
        json!({"model":"alias","input":"hello"}),
    )
    .await;
    assert_eq!(status, 200);
    let events = drain(&mut rx);
    assert!(events.iter().any(|e| matches!(e, TelemetryEvent::ProviderAttemptStarted { .. })));
    assert!(events.iter().any(
        |e| matches!(e, TelemetryEvent::ProviderAttemptCompleted { status: ai_gateway::telemetry::events::AttemptStatus::Success, .. })
    ));
    assert!(events.iter().any(|e| matches!(e, TelemetryEvent::UsageRecorded { .. })));
}
#[tokio::test]
async fn embeddings_failure_must_record_failed_attempt() {
    let (state, mut rx) = setup_embeddings(true);
    let (status, _) = post(
        state,
        "/v1/embeddings",
        json!({"model":"alias","input":"hello"}),
    )
    .await;
    assert_ne!(status, 200);
    let events = drain(&mut rx);
    assert!(events.iter().any(
        |e| matches!(e, TelemetryEvent::ProviderAttemptCompleted { status: ai_gateway::telemetry::events::AttemptStatus::Failed, .. })
    ));
    assert!(events.iter().any(|e| matches!(e, TelemetryEvent::RequestCompleted { success: false, .. })));
}

#[tokio::test]
async fn stream_failure_must_be_reported_as_failure() {
    let (state, mut rx, _) = setup(2, true);
    let (_, body) = post(
        state,
        "/v1/chat/completions",
        json!({"model":"first/m","stream":true,"messages":[]}),
    )
    .await;
    let events = drain(&mut rx);
    let failure_recorded = events
        .iter()
        .any(|e| matches!(e, TelemetryEvent::RequestCompleted { success: false, .. }));
    assert!(
        body.contains("audit failure") && failure_recorded,
        "failure_recorded={failure_recorded}; client received: {body}"
    );
}
#[tokio::test]
async fn failed_stream_must_preserve_partial_content() {
    let (state, mut rx, _) = setup(2, true);
    post(
        state,
        "/v1/chat/completions",
        json!({"model":"first/m","stream":true,"messages":[]}),
    )
    .await;
    assert!(drain(&mut rx).iter().any(|e| matches!(e, TelemetryEvent::GenerationContent {content,..} if content == "partial output")));
}
#[tokio::test]
async fn http_failure_must_record_an_attempt() {
    let (state, mut rx, _) = setup(1, true);
    post(
        state,
        "/v1/chat/completions",
        json!({"model":"first/m","messages":[]}),
    )
    .await;
    assert!(
        drain(&mut rx)
            .iter()
            .any(|e| matches!(e, TelemetryEvent::ProviderAttemptCompleted { .. }))
    );
}
#[tokio::test]
async fn empty_fallback_policy_must_not_use_second_provider() {
    let (state, _, seen) = setup(1, true);
    let (status, _) = post(
        state,
        "/v1/chat/completions",
        json!({"model":"alias","messages":[]}),
    )
    .await;
    assert_ne!(
        status,
        200,
        "Unexpected successful fallback; calls: {}",
        seen.lock().unwrap().len()
    );
}
#[tokio::test]
async fn disabled_provider_must_not_execute() {
    let (state, _, seen) = setup(0, false);
    post(
        state,
        "/v1/chat/completions",
        json!({"model":"first/m","messages":[]}),
    )
    .await;
    assert!(seen.lock().unwrap().is_empty());
}
#[tokio::test]
async fn chat_completions_must_return_all_choices_for_n_greater_than_one() {
    // Both choices from a multi-choice provider response must reach the
    // client's JSON body, keyed by their own index — not merged into one
    // choice or dropped (docs/REMEDIATION_PLAN.md Phase 2.3 / Audit A08).
    let (state, _, _) = setup(3, true);
    let (status, body) = post(
        state,
        "/v1/chat/completions",
        json!({"model":"alias","messages":[{"role":"user","content":"hi"}],"n":2}),
    )
    .await;
    assert_eq!(status, 200);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    let choices = parsed["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0]["index"], 0);
    assert_eq!(choices[0]["message"]["content"], "partial output");
    assert_eq!(choices[1]["index"], 1);
    assert_eq!(choices[1]["message"]["content"], "second choice output");
}
#[tokio::test]
async fn telemetry_emits_separate_generation_content_per_choice_for_n_greater_than_one() {
    // docs/frontend/API_GAPS.md item 5's choice-ID gap: telemetry must
    // persist one GenerationContent event per choice, not one event
    // merging every choice of an `n > 1` attempt together.
    let (state, mut rx, _) = setup(3, true);
    let (status, _) = post(
        state,
        "/v1/chat/completions",
        json!({"model":"alias","messages":[{"role":"user","content":"hi"}],"n":2}),
    )
    .await;
    assert_eq!(status, 200);

    let events = drain(&mut rx);
    let mut generations: Vec<(u32, String, String)> = events
        .iter()
        .filter_map(|e| match e {
            TelemetryEvent::GenerationContent { choice_index, content, status, .. } => {
                Some((*choice_index, content.clone(), status.clone()))
            }
            _ => None,
        })
        .collect();
    generations.sort_by_key(|(idx, ..)| *idx);

    assert_eq!(
        generations,
        vec![
            (0, "partial output".to_string(), "complete".to_string()),
            (1, "second choice output".to_string(), "complete".to_string()),
        ]
    );
}
#[tokio::test]
async fn telemetry_preserves_partial_content_per_choice_on_mid_stream_error() {
    // The same choice-separation must hold on the partial-content-on-error
    // path (Audit A04), not just the clean-completion path.
    let (state, mut rx, _) = setup(4, true);
    post(
        state,
        "/v1/chat/completions",
        json!({"model":"alias","messages":[{"role":"user","content":"hi"}],"n":2}),
    )
    .await;

    let events = drain(&mut rx);
    let mut generations: Vec<(u32, String, String)> = events
        .iter()
        .filter_map(|e| match e {
            TelemetryEvent::GenerationContent { choice_index, content, status, .. } => {
                Some((*choice_index, content.clone(), status.clone()))
            }
            _ => None,
        })
        .collect();
    generations.sort_by_key(|(idx, ..)| *idx);

    assert_eq!(
        generations,
        vec![
            (0, "partial output".to_string(), "partial".to_string()),
            (1, "second choice output".to_string(), "partial".to_string()),
        ]
    );
}
#[tokio::test]
async fn responses_must_forward_message_object_input() {
    let (state, _, seen) = setup(0, true);
    post(
        state,
        "/v1/responses",
        json!({"model":"first/m","input":[{"role":"user","content":"Keep this prompt"}]}),
    )
    .await;
    let calls = seen.lock().unwrap();
    assert!(!calls[0].messages.as_ref().unwrap().is_empty());
}
#[tokio::test]
async fn responses_must_stream_real_sse_when_requested() {
    // `stream: true` previously always collected the full response
    // server-side and returned one JSON object regardless of the caller's
    // request (docs/REMEDIATION_PLAN.md Phase 2.2 / Audit A09). Now it must
    // return real SSE: a `response.created` event, a text delta event
    // carrying the provider's content, and a terminal `response.completed`
    // event — not a single `application/json` body.
    let (state, _, _) = setup(0, true);
    let (status, body) = post(
        state,
        "/v1/responses",
        json!({"model":"first/m","input":"hi","stream":true}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        body.contains("event: response.created"),
        "missing response.created event: {body}"
    );
    assert!(
        body.contains("event: response.output_text.delta") && body.contains("partial output"),
        "missing output_text.delta event with provider content: {body}"
    );
    assert!(
        body.contains("event: response.completed"),
        "missing response.completed event: {body}"
    );
}
#[tokio::test]
async fn responses_must_honor_stream() {
    let (state, _, seen) = setup(0, true);
    post(
        state,
        "/v1/responses",
        json!({"model":"first/m","input":"hi","stream":true}),
    )
    .await;
    assert!(seen.lock().unwrap()[0].stream);
}
#[tokio::test]
async fn responses_must_extract_reasoning_effort_from_nested_object() {
    // The Responses API's own shape is `reasoning: {"effort": "..."}`,
    // distinct from Chat Completions' flat `reasoning_effort` field — this
    // previously had no test coverage of its own
    // (docs/REMEDIATION_PLAN.md Phase 2.3).
    let (state, _, seen) = setup(0, true);
    post(
        state,
        "/v1/responses",
        json!({"model":"first/m","input":"hi","reasoning":{"effort":"high"}}),
    )
    .await;
    assert_eq!(
        seen.lock().unwrap()[0].reasoning_effort.as_deref(),
        Some("high")
    );
}
#[test]
fn gemini_endpoint_must_match_documented_base() {
    let mut config = pc("google", true);
    config.base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
        .parse()
        .unwrap();
    let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();
    assert_eq!(
        adapter.endpoint("/v1/chat/completions"),
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
    );
}
#[test]
fn unversioned_bases_still_get_v1_appended() {
    // llama.cpp / OpenRouter-style bases have no version segment at all and
    // must still get the full "/v1/..." path appended.
    for base in [
        "http://localhost:8080",
        "https://openrouter.ai/api",
        "http://localhost:8000",
    ] {
        let mut config = pc("plain", true);
        config.base_url = base.parse().unwrap();
        let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();
        assert_eq!(
            adapter.endpoint("/v1/chat/completions"),
            format!("{base}/v1/chat/completions")
        );
    }
}
#[test]
fn literal_v1_suffixed_base_does_not_double_up() {
    let mut config = pc("hf", true);
    config.base_url = "https://router.huggingface.co/v1".parse().unwrap();
    let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();
    assert_eq!(
        adapter.endpoint("/v1/chat/completions"),
        "https://router.huggingface.co/v1/chat/completions"
    );
}
#[test]
fn reasoning_controls_must_reach_provider() {
    let adapter =
        OpenAICompatibleAdapter::new(&pc("first", true), ModelCapabilities::default()).unwrap();
    let req: GatewayRequest =
        serde_json::from_value(json!({"model":"m","reasoning":{"enabled":true,"effort":"high"}}))
            .unwrap();
    let body = adapter.build_chat_body(&req, "m");
    assert!(body.get("reasoning").is_some() || body.get("reasoning_effort").is_some());
}
#[tokio::test]
async fn raw_request_must_preserve_unknown_fields() {
    let (state, mut rx, _) = setup(0, true);
    post(
        state,
        "/v1/chat/completions",
        json!({"model":"first/m","messages":[],"future_provider_field":"preserve me"}),
    )
    .await;
    let events = drain(&mut rx);
    let raw = events
        .iter()
        .find_map(|e| match e {
            TelemetryEvent::RequestReceived { raw_request, .. } => raw_request.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(raw["future_provider_field"], "preserve me");
}

#[tokio::test]
async fn database_batch_must_preserve_chunks_for_new_requests() {
    // Audit A02, fixed in docs/REMEDIATION_PLAN.md Phase 0.1 and verified
    // against a real, disposable Postgres (see that phase's verification
    // note). This test needs a live database this environment doesn't
    // provide by default, so it degrades to a no-op rather than failing
    // `cargo test --all-targets` for everyone — set `AUDIT_DATABASE_URL` to
    // an isolated, disposable Postgres to actually exercise it, e.g.:
    //   docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16
    //   AUDIT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/postgres \
    //     cargo test --test audit_contract_tests database_batch_must_preserve_chunks
    let Ok(url) = std::env::var("AUDIT_DATABASE_URL") else {
        eprintln!("skipping database_batch_must_preserve_chunks_for_new_requests: AUDIT_DATABASE_URL not set");
        return;
    };
    let writer = ai_gateway::telemetry::TelemetryWriter::new(&url, 2, 100, 60000, true, true)
        .await
        .unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let (state, mut rx, _) = setup(0, true);
    for _ in 0..10 {
        post(
            state.clone(),
            "/v1/chat/completions",
            json!({"model":"first/m","messages":[]}),
        )
        .await;
    }
    let events = drain(&mut rx);
    let ids: Vec<uuid::Uuid> = events
        .iter()
        .filter_map(|e| match e {
            TelemetryEvent::RequestReceived { request_id, .. } => Some(*request_id),
            _ => None,
        })
        .collect();
    let (tx, receiver) = tokio::sync::mpsc::channel(200);
    for e in events {
        tx.send(e).await.unwrap();
    }
    drop(tx);
    writer.run(receiver).await;
    let parents: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inference_requests WHERE request_id = ANY($1)")
            .bind(&ids)
            .fetch_one(&pool)
            .await
            .unwrap();
    let chunks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM stream_events WHERE request_id = ANY($1)")
            .bind(&ids)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(parents, 10);
    assert_eq!(
        chunks, 10,
        "Parent records persist, but chunks in the same batch are lost"
    );
}

#[tokio::test]
async fn response_cache_serves_a_repeated_non_streaming_request_without_a_second_dispatch() {
    // Exact-match caching (docs/REMEDIATION_PLAN.md Phase 5 caching work):
    // an identical non-streaming request within TTL must be served from
    // cache — the provider's `execute` must not be invoked a second time —
    // and the response must carry the x-cache: HIT header.
    let mut config: GatewayConfig = serde_yaml::from_str("{}").unwrap();
    config.routing.max_retries_per_provider = 1;
    config.routing.fallback_on.clear();
    let seen = Arc::new(Mutex::new(vec![]));
    let mut providers = ProviderRegistry::new();
    providers.register(
        pc("first", true),
        Arc::new(ProbeProvider {
            name: "first".into(),
            mode: 0,
            caps: ModelCapabilities::default(),
            seen: seen.clone(),
        }),
    );
    let models = serde_json::from_value(
        json!({"alias": {"name":"alias","candidates":[{"provider":"first","model":"m","priority":1}]}}),
    )
    .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(100);
    let state = Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(models),
        provider_registry: providers,
        router: RoutingEngine::new("ordered".into()),
        telemetry_tx: tx,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: std::sync::Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: Some(Arc::new(ai_gateway::cache::ResponseCache::new(
            std::time::Duration::from_secs(60),
            100,
        ))),
        analytics_pool: None,
    });

    let body = json!({"model":"alias","messages":[{"role":"user","content":"hello"}],"stream":false});

    let (status1, _body1) = post(state.clone(), "/v1/chat/completions", body.clone()).await;
    assert_eq!(status1, 200);
    assert_eq!(seen.lock().unwrap().len(), 1, "first request must dispatch to the provider");

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get("x-cache").map(|v| v.to_str().unwrap()),
        Some("HIT"),
        "second identical request must be served from cache"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a cache hit must not dispatch to the provider a second time"
    );

    // A request with different content must miss and dispatch normally.
    let different_body = json!({"model":"alias","messages":[{"role":"user","content":"different"}],"stream":false});
    let (status3, _body3) = post(state.clone(), "/v1/chat/completions", different_body).await;
    assert_eq!(status3, 200);
    assert_eq!(seen.lock().unwrap().len(), 2, "a different request must not hit the first request's cache entry");
}
