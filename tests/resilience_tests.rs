//! Resilience behaviour, end to end (HTTP in -> gateway -> mock upstreams over
//! real TCP):
//!
//! * a streaming response that fails or stalls *before any output* is failed
//!   over to the next candidate instead of reaching the client broken or blank,
//!   while one that fails *after* output is never replayed,
//! * several API keys per provider are rotated, benched on 429/401/403 and
//!   retried within the same request.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};
use ai_gateway::types::{ModelDefinition, ProviderConfig};

// ─── Mock upstream ───────────────────────────────────────────────────────────

/// A scripted reply: status, headers, body. The header `x-stall` makes it a
/// 200 event stream that never produces a byte.
type Reply = (u16, Vec<(&'static str, &'static str)>, String);

#[derive(Default)]
struct Upstream {
    hits: Mutex<usize>,
    /// Replies consumed front-to-back before falling back to a normal 200.
    script: Mutex<VecDeque<Reply>>,
    /// The `Authorization` header of every request, in order.
    auths: Mutex<Vec<String>>,
}

async fn upstream_handler(State(up): State<Arc<Upstream>>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    *up.hits.lock().unwrap() += 1;
    up.auths
        .lock()
        .unwrap()
        .push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string());

    if let Some((status, extra, text)) = up.script.lock().unwrap().pop_front() {
        if extra.iter().any(|(k, _)| *k == "x-stall") {
            let pending = futures::stream::pending::<Result<Bytes, std::convert::Infallible>>();
            return ([("content-type", "text/event-stream")], Body::from_stream(pending)).into_response();
        }
        let mut response = (StatusCode::from_u16(status).unwrap(), text).into_response();
        for (name, value) in extra {
            response.headers_mut().insert(name, value.parse().unwrap());
        }
        return response;
    }

    let stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    let _ = uri;
    if stream {
        let sse = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    axum::Json(serde_json::json!({
        "id": "c1", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    }))
    .into_response()
}

async fn start_upstream() -> (String, Arc<Upstream>) {
    let up = Arc::new(Upstream::default());
    let app = Router::new().fallback(upstream_handler).with_state(up.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), up)
}

fn hits(up: &Upstream) -> usize {
    *up.hits.lock().unwrap()
}

fn auths(up: &Upstream) -> Vec<String> {
    up.auths.lock().unwrap().clone()
}

// ─── Gateway under test ──────────────────────────────────────────────────────

/// A gateway with the given mock providers and one alias, `chain`, whose
/// candidates are those providers in order.
fn gateway_chain(providers: &[(&str, &str, &str)], tune: impl FnOnce(&mut GatewayConfig)) -> Router {
    let mut provider_registry = ProviderRegistry::new();
    let mut candidates = String::new();
    for (i, (name, base, extra)) in providers.iter().enumerate() {
        let config: ProviderConfig = serde_yaml::from_str(&format!(
            "name: {name}\ntype: open_ai_compatible\nbase_url: \"{base}\"\ntimeout_secs: 10\n{extra}"
        ))
        .unwrap();
        let adapter = providers::build_adapter(&config).unwrap();
        provider_registry.register(config, adapter);
        candidates.push_str(&format!("  - provider: {name}\n    model: m\n    priority: {}\n", i + 1));
    }
    let definition: ModelDefinition =
        serde_yaml::from_str(&format!("name: chain\ncandidates:\n{candidates}")).unwrap();

    let mut config = GatewayConfig::load(None).unwrap();
    tune(&mut config);
    let (telemetry_tx, rx) = tokio::sync::mpsc::channel(1000);
    std::mem::forget(rx);
    build_router(Arc::new(AppState {
        config,
        model_registry: ModelRegistry::new(HashMap::from([("chain".to_string(), definition)])),
        provider_registry,
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
    }))
}

async fn post(app: &Router, body: serde_json::Value) -> (StatusCode, HeaderMap, String) {
    let response = app
        .clone()
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
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

fn chat(stream: bool) -> serde_json::Value {
    serde_json::json!({"model": "chain", "stream": stream, "messages": [{"role": "user", "content": "ping"}]})
}

fn tune_fast(c: &mut GatewayConfig) {
    c.routing.max_retries_per_provider = 1;
}

fn sse(body: &str) -> Reply {
    (200, vec![("content-type", "text/event-stream")], body.to_string())
}

fn rate_limited(retry_after: &'static str) -> Reply {
    (
        429,
        vec![("retry-after", retry_after)],
        r#"{"error":{"message":"slow down"}}"#.to_string(),
    )
}

fn rejected() -> Reply {
    (401, vec![], r#"{"error":{"message":"bad key"}}"#.to_string())
}

const ERROR_FRAME: &str = "data: {\"error\":{\"code\":429,\"message\":\"upstream slow down\"}}\n\n";
const EMPTY_STREAM: &str = "data: [DONE]\n\n";

// ─── Stream failover (failure before any output) ─────────────────────────────

#[tokio::test]
async fn an_error_frame_before_any_output_fails_over_to_the_next_provider() {
    let (a_base, a) = start_upstream().await;
    let (b_base, b) = start_upstream().await;
    a.script.lock().unwrap().push_back(sse(ERROR_FRAME));
    let app = gateway_chain(&[("a", &a_base, ""), ("b", &b_base, "")], tune_fast);

    let (status, headers, text) = post(&app, chat(true)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(headers["x-gateway-provider"], "b", "{text}");
    assert_eq!(headers["x-gateway-attempts"], "2");
    assert!(text.contains("\"hi\""), "the client gets b's answer: {text}");
    assert!(!text.contains("upstream slow down"), "a's failure must not leak into the stream: {text}");
    assert_eq!((hits(&a), hits(&b)), (1, 1));
}

#[tokio::test]
async fn a_stream_that_ends_empty_fails_over_instead_of_returning_a_blank_answer() {
    let (a_base, a) = start_upstream().await;
    let (b_base, _b) = start_upstream().await;
    a.script.lock().unwrap().push_back(sse(EMPTY_STREAM));
    let app = gateway_chain(&[("a", &a_base, ""), ("b", &b_base, "")], tune_fast);

    let (status, headers, text) = post(&app, chat(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-gateway-provider"], "b");
    assert!(text.contains("\"hi\""), "{text}");
}

#[tokio::test]
async fn a_stalled_stream_is_abandoned_after_the_first_token_timeout() {
    let (a_base, a) = start_upstream().await;
    let (b_base, _b) = start_upstream().await;
    a.script.lock().unwrap().push_back((200, vec![("x-stall", "1")], String::new()));
    let app = gateway_chain(&[("a", &a_base, ""), ("b", &b_base, "")], |c| {
        tune_fast(c);
        c.routing.first_token_timeout_secs = Some(1);
    });

    let started = Instant::now();
    let (status, headers, _) = post(&app, chat(true)).await;
    let took = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-gateway-provider"], "b");
    assert!(took >= Duration::from_millis(900), "waited for the timeout: {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
}

#[tokio::test]
async fn with_no_other_candidate_a_pre_output_failure_is_an_error_not_a_blank_200() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(sse(ERROR_FRAME));
    let app = gateway_chain(&[("only", &base, "")], tune_fast);

    let (status, _, text) = post(&app, chat(true)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{text}");
    assert!(text.contains("upstream slow down"), "{text}");
}

#[tokio::test]
async fn once_output_has_started_a_failure_is_not_hidden_or_replayed() {
    let (a_base, a) = start_upstream().await;
    let (b_base, b) = start_upstream().await;
    a.script.lock().unwrap().push_back(sse(concat!(
        "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        "data: {\"error\":{\"code\":500,\"message\":\"died mid-answer\"}}\n\n",
    )));
    let app = gateway_chain(&[("a", &a_base, ""), ("b", &b_base, "")], tune_fast);

    let (status, headers, text) = post(&app, chat(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-gateway-provider"], "a");
    assert!(text.contains("partial"), "{text}");
    assert!(text.contains("died mid-answer"), "the client must be told the answer is incomplete: {text}");
    assert_eq!(hits(&b), 0, "replaying on b would corrupt the answer the client has begun to receive");
}

#[tokio::test]
async fn stream_failover_can_be_turned_off() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(sse(ERROR_FRAME));
    let app = gateway_chain(&[("only", &base, "")], |c| {
        tune_fast(c);
        c.routing.stream_failover = false;
    });

    let (status, _, text) = post(&app, chat(true)).await;
    // Previous behaviour: the response starts at once and carries the error.
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("upstream slow down"), "{text}");
}

// ─── Several API keys per provider ───────────────────────────────────────────

#[tokio::test]
async fn a_rate_limited_key_is_replaced_by_the_next_one_within_the_same_request() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(rate_limited("30"));
    let app = gateway_chain(&[("p", &base, "api_key: \"k1,k2\"\n")], tune_fast);

    let (status, headers, _) = post(&app, chat(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-gateway-attempts"], "1", "key rotation is invisible to routing");
    assert_eq!(auths(&up), vec!["Bearer k1", "Bearer k2"]);

    // k1 stays benched for the next requests (30s), so they all use k2.
    for _ in 0..2 {
        post(&app, chat(false)).await;
    }
    assert_eq!(auths(&up)[2..], ["Bearer k2", "Bearer k2"]);
}

#[tokio::test]
async fn a_rejected_key_is_skipped_and_the_request_still_succeeds() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(rejected());
    let app = gateway_chain(&[("p", &base, "api_key: \"stale,good\"\n")], tune_fast);

    let (status, _, text) = post(&app, chat(true)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(auths(&up), vec!["Bearer stale", "Bearer good"]);
}

#[tokio::test]
async fn keys_are_spread_round_robin_when_all_are_healthy() {
    let (base, up) = start_upstream().await;
    let app = gateway_chain(&[("p", &base, "api_key: \"a,b,c\"\n")], tune_fast);
    for _ in 0..6 {
        post(&app, chat(false)).await;
    }
    assert_eq!(
        auths(&up),
        ["Bearer a", "Bearer b", "Bearer c", "Bearer a", "Bearer b", "Bearer c"]
    );
}

#[tokio::test]
async fn when_every_key_is_rate_limited_the_soonest_recovery_sets_the_cooldown() {
    let (base, up) = start_upstream().await;
    // k1 says 1s, k2 says 120s. The provider cooldown must reflect k1 (the
    // best key), not k2 (the last one tried): a request after ~1s goes through.
    up.script.lock().unwrap().push_back(rate_limited("1"));
    up.script.lock().unwrap().push_back(rate_limited("120"));
    let app = gateway_chain(&[("p", &base, "api_key: \"k1,k2\"\n")], |c| {
        tune_fast(c);
        c.routing.max_retry_after_secs = 1;
    });

    let started = Instant::now();
    let (status, _, text) = post(&app, chat(false)).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_ne!(status, StatusCode::OK, "{text}");
    assert_eq!(hits(&up), 2, "each key tried exactly once");

    // The provider is cooling down right now: no further upstream calls...
    let (status, _, _) = post(&app, chat(false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(hits(&up), 2);

    // ...but only until the best key is back, not for the 120s the last one asked.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let (status, _, text) = post(&app, chat(false)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(hits(&up), 3);
}

#[tokio::test]
async fn a_single_key_is_never_benched_locally() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(rate_limited("1"));
    let app = gateway_chain(&[("p", &base, "api_key: \"only\"\n")], |c| {
        c.routing.max_retries_per_provider = 2;
        c.routing.max_retry_after_secs = 5;
    });

    let (status, _, _) = post(&app, chat(false)).await;
    assert_eq!(status, StatusCode::OK, "waited out the 1s Retry-After and retried with the same key");
    assert_eq!(auths(&up), vec!["Bearer only", "Bearer only"]);
}
