//! Config hot-reload, end to end: a real `gateway.yaml` (and `.env`) on disk, a
//! gateway booted from it the way `main` does, mock upstreams over TCP, and the
//! real `POST /admin/reload` / file watcher driving changes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;

use ai_gateway::config::GatewayConfig;
use ai_gateway::config::env::EnvFile;
use ai_gateway::providers;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::reload::Reloader;
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};

// ─── Mock upstream ───────────────────────────────────────────────────────────

#[derive(Default)]
struct Upstream {
    hits: Mutex<usize>,
    auths: Mutex<Vec<String>>,
    /// Status codes returned (with `retry-after: 60`) before succeeding.
    script: Mutex<VecDeque<u16>>,
}

async fn upstream_handler(State(up): State<Arc<Upstream>>, headers: HeaderMap, body: Bytes) -> Response {
    *up.hits.lock().unwrap() += 1;
    up.auths
        .lock()
        .unwrap()
        .push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string());
    if let Some(status) = up.script.lock().unwrap().pop_front() {
        return (StatusCode::from_u16(status).unwrap(), [("retry-after", "60")], r#"{"error":{"message":"no"}}"#)
            .into_response();
    }
    let stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    if stream {
        let sse = concat!(
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }
    axum::Json(serde_json::json!({
        "id": "c", "object": "chat.completion", "created": 0, "model": "m",
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

// ─── Booting a gateway from files ────────────────────────────────────────────

struct Booted {
    app: Router,
    state: Arc<AppState>,
    dir: PathBuf,
}

impl Booted {
    fn config_path(&self) -> PathBuf {
        self.dir.join("gateway.yaml")
    }
    fn write_config(&self, yaml: &str) {
        std::fs::write(self.config_path(), yaml).unwrap();
    }
}

impl Drop for Booted {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Provider entries for a YAML `providers:` block: `(name, base_url, extra lines)`.
fn yaml(providers: &[(&str, &str, &str)], models: &[(&str, &str)], extra_top_level: &str) -> String {
    let mut out = String::from("providers:\n");
    for (name, base, extra) in providers {
        out.push_str(&format!(
            "  {name}:\n    name: {name}\n    type: open_ai_compatible\n    base_url: \"{base}\"\n    timeout_secs: 10\n{extra}"
        ));
    }
    out.push_str("models:\n");
    for (alias, provider) in models {
        out.push_str(&format!(
            "  {alias}:\n    name: {alias}\n    candidates:\n      - provider: {provider}\n        model: m\n        priority: 1\n"
        ));
    }
    out.push_str(extra_top_level);
    out
}

/// Start a gateway from `config_yaml` written to a temp dir, wired like `main`
/// (same adapter builder, a `Reloader` for the file). `env` = `(.env contents)`
/// turns on env-file reloading too.
fn boot(config_yaml: &str, env_file_contents: Option<&str>) -> Booted {
    let dir = std::env::temp_dir().join(format!("gw-reload-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gateway.yaml");
    std::fs::write(&path, config_yaml).unwrap();

    let env = env_file_contents.map(|contents| {
        let env_path = dir.join(".env");
        std::fs::write(&env_path, contents).unwrap();
        let file = EnvFile::new(env_path, []);
        file.reload().unwrap(); // what startup's load does, via the overlay
        file
    });

    let config = GatewayConfig::from_file(&path).unwrap();
    let mut provider_registry = ProviderRegistry::new();
    for (name, pc) in &config.providers {
        let mut pc = pc.clone();
        if pc.name.is_empty() {
            pc.name = name.clone();
        }
        let adapter = providers::build_registered_adapter(&pc, None).unwrap();
        provider_registry.register(pc, adapter);
    }
    let (telemetry_tx, rx) = tokio::sync::mpsc::channel(1000);
    std::mem::forget(rx);
    let reloader = Arc::new(Reloader::new(Some(path.clone()), env, &config));
    let state = Arc::new(AppState {
        model_registry: ModelRegistry::new(config.models.clone()),
        router: RoutingEngine::new(config.routing.default_policy.clone()),
        config: config.into(),
        provider_registry,
        telemetry_tx,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
        reloader: Some(reloader),
    });
    Booted { app: build_router(state.clone()), state, dir }
}

async fn call(app: &Router, method: &str, path: &str, body: Option<serde_json::Value>, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(path).header("content-type", "application/json");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
}

async fn chat(app: &Router, model: &str, stream: bool) -> (StatusCode, HeaderMap, String) {
    call(
        app,
        "POST",
        "/v1/chat/completions",
        Some(serde_json::json!({"model": model, "stream": stream, "messages": [{"role": "user", "content": "hi"}]})),
        &[],
    )
    .await
}

async fn reload(app: &Router) -> (StatusCode, serde_json::Value) {
    let (status, _, body) = call(app, "POST", "/admin/reload", None, &[]).await;
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

fn names(report: &serde_json::Value, group: &str, kind: &str) -> Vec<String> {
    report[group][kind]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn exists(path: &Path) -> bool {
    path.exists()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_reload_adds_a_provider_and_an_alias_without_a_restart() {
    let (a_base, _a) = start_upstream().await;
    let (b_base, b) = start_upstream().await;
    let gw = boot(&yaml(&[("a", &a_base, "")], &[("old", "a")], ""), None);

    let (status, _, _) = chat(&gw.app, "new", false).await;
    assert_ne!(status, StatusCode::OK, "alias does not exist yet");

    gw.write_config(&yaml(&[("a", &a_base, ""), ("b", &b_base, "")], &[("old", "a"), ("new", "b")], ""));
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(names(&report, "providers", "added"), ["b"]);
    assert_eq!(names(&report, "models", "added"), ["new"]);
    assert_eq!(report["applied"], true);

    let (status, headers, text) = chat(&gw.app, "new", false).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(headers["x-gateway-provider"], "b");
    assert_eq!(hits(&b), 1);
    // The untouched alias still works.
    assert_eq!(chat(&gw.app, "old", false).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_reload_removes_a_provider_and_an_alias() {
    let (a_base, _a) = start_upstream().await;
    let (b_base, _b) = start_upstream().await;
    let gw = boot(&yaml(&[("a", &a_base, ""), ("b", &b_base, "")], &[("keep", "a"), ("drop", "b")], ""), None);
    assert_eq!(chat(&gw.app, "drop", false).await.0, StatusCode::OK);

    gw.write_config(&yaml(&[("a", &a_base, "")], &[("keep", "a")], ""));
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(names(&report, "providers", "removed"), ["b"]);
    assert_eq!(names(&report, "models", "removed"), ["drop"]);

    assert_ne!(chat(&gw.app, "drop", false).await.0, StatusCode::OK);
    assert_eq!(chat(&gw.app, "keep", false).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_bad_config_is_rejected_and_the_running_one_keeps_serving() {
    let (a_base, a) = start_upstream().await;
    let gw = boot(&yaml(&[("a", &a_base, "")], &[("ok", "a")], ""), None);

    for bad in [
        "providers: [this is: not valid".to_string(),
        yaml(&[("a", &a_base, "")], &[("ok", "a")], "routing:\n  default_policy: no_such_policy\n"),
        yaml(&[("a", &a_base, "    max_concurrent: 0\n")], &[("ok", "a")], ""),
    ] {
        gw.write_config(&bad);
        let (status, report) = reload(&gw.app).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{report}");
        assert_eq!(report["ok"], false);
        assert!(report["error"].as_str().is_some_and(|e| !e.is_empty()), "{report}");

        let (status, _, _) = chat(&gw.app, "ok", false).await;
        assert_eq!(status, StatusCode::OK, "old config must keep serving after: {report}");
    }
    assert_eq!(hits(&a), 3);

    // Status endpoint remembers the rejection.
    let (status, _, body) = call(&gw.app, "GET", "/admin/reload", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    let status_json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status_json["last"]["ok"], false);
    assert_eq!(status_json["reloads_applied"], 0);
}

#[tokio::test]
async fn an_in_flight_stream_survives_its_provider_being_removed() {
    let (a_base, _a) = start_upstream().await;
    let (b_base, _b) = start_upstream().await;
    let gw = boot(&yaml(&[("a", &a_base, ""), ("b", &b_base, "")], &[("s", "a"), ("other", "b")], ""), None);

    // Open the stream (response returned, body not yet read) ...
    let response = gw
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "s", "stream": true, "messages": [{"role": "user", "content": "hi"}]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // ... remove its provider and alias ...
    gw.write_config(&yaml(&[("b", &b_base, "")], &[("other", "b")], ""));
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(names(&report, "providers", "removed"), ["a"]);

    // ... and the stream still completes.
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("hel") && text.contains("lo") && text.contains("[DONE]"), "{text}");
}

#[tokio::test]
async fn unchanged_providers_keep_their_counters_and_api_key_state() {
    let (base, up) = start_upstream().await;
    up.script.lock().unwrap().push_back(429); // first key gets rate limited
    let provider = |extra: &str| yaml(&[("p", &base, &format!("    api_key: \"k1,k2\"\n{extra}"))], &[("m", "p")], "");
    let gw = boot(&provider(""), None);

    assert_eq!(chat(&gw.app, "m", false).await.0, StatusCode::OK);
    assert_eq!(auths(&up), ["Bearer k1", "Bearer k2"], "k1 limited, request rotated to k2");
    let before = gw.state.provider_registry.all_statuses().into_iter().find(|s| s.name == "p").unwrap().total_requests;
    assert!(before >= 1);

    // Reload with only an unrelated change (a new alias): provider p must be untouched.
    gw.write_config(&yaml(&[("p", &base, "    api_key: \"k1,k2\"\n")], &[("m", "p"), ("extra", "p")], ""));
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert!(names(&report, "providers", "updated").is_empty() && names(&report, "providers", "added").is_empty(), "{report}");

    let after = gw.state.provider_registry.all_statuses().into_iter().find(|s| s.name == "p").unwrap().total_requests;
    assert_eq!(after, before, "counters survive a reload");

    // k1 is still benched (60s), so the next request goes straight to k2.
    assert_eq!(chat(&gw.app, "m", false).await.0, StatusCode::OK);
    assert_eq!(auths(&up).last().unwrap(), "Bearer k2");
    assert_eq!(auths(&up).len(), 3, "no extra attempt on the benched key");
}

#[tokio::test]
async fn editing_the_env_file_adds_an_api_key_on_reload() {
    let (base, up) = start_upstream().await;
    let var = format!("GW_RELOAD_KEY_{}", uuid::Uuid::now_v7().simple());
    let gw = boot(
        &yaml(&[("p", &base, &format!("    api_key_env: {var}\n"))], &[("m", "p")], ""),
        Some(&format!("{var}=only-key\n")),
    );

    assert_eq!(chat(&gw.app, "m", false).await.0, StatusCode::OK);
    assert_eq!(auths(&up), ["Bearer only-key"]);

    std::fs::write(gw.dir.join(".env"), format!("{var}=only-key,second-key\n")).unwrap();
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["env_changed"], serde_json::json!([var]));
    assert_eq!(names(&report, "providers", "updated"), ["p"], "key change rebuilds the adapter");
    assert!(!report.to_string().contains("second-key"), "values must never be reported");

    for _ in 0..4 {
        assert_eq!(chat(&gw.app, "m", false).await.0, StatusCode::OK);
    }
    let used = auths(&up);
    assert!(used.contains(&"Bearer second-key".to_string()), "{used:?}");
}

#[tokio::test]
async fn sections_that_need_a_restart_are_reported_and_not_applied() {
    let (base, _up) = start_upstream().await;
    let gw = boot(&yaml(&[("p", &base, "")], &[("m", "p")], ""), None);
    let before = gw.state.config();
    assert!(!before.caching.enabled);
    assert_eq!(before.routing.max_retries_per_provider, 2);

    gw.write_config(&yaml(
        &[("p", &base, "")],
        &[("m", "p")],
        "caching:\n  enabled: true\nserver:\n  port: 12345\nrouting:\n  max_retries_per_provider: 5\n",
    ));
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let mut restart: Vec<String> =
        report["restart_required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    restart.sort();
    assert_eq!(restart, ["caching", "server"]);

    let now = gw.state.config();
    assert!(!now.caching.enabled, "caching stays as running");
    assert_ne!(now.server.port, 12345, "server stays as running");
    assert_eq!(now.routing.max_retries_per_provider, 5, "routing does reload");
    assert_eq!(report["applied"], true);
}

#[tokio::test]
async fn a_reload_with_no_changes_says_so() {
    let (base, _up) = start_upstream().await;
    let gw = boot(&yaml(&[("p", &base, "")], &[("m", "p")], ""), None);
    let (status, report) = reload(&gw.app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["applied"], false);
    assert_eq!(report["ok"], true);
}

#[tokio::test]
async fn the_reload_endpoint_refuses_browsers_and_foreign_hosts() {
    let (base, _up) = start_upstream().await;
    let gw = boot(&yaml(&[("p", &base, "")], &[("m", "p")], ""), None);

    for (method, headers) in [
        ("POST", vec![("origin", "https://evil.example")]),
        ("GET", vec![("origin", "https://evil.example")]),
        ("POST", vec![("sec-fetch-site", "cross-site")]),
        ("POST", vec![("host", "evil.example")]),
    ] {
        let (status, _, body) = call(&gw.app, method, "/admin/reload", None, &headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {headers:?}: {body}");
    }
    // A plain local caller is fine.
    let (status, _, _) = call(&gw.app, "POST", "/admin/reload", None, &[("host", "localhost:8999")]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn without_a_reloader_the_endpoint_says_it_is_unavailable() {
    let (base, _up) = start_upstream().await;
    let gw = boot(&yaml(&[("p", &base, "")], &[("m", "p")], ""), None);
    // Rebuild the same state with no reloader.
    let state = Arc::new(AppState {
        config: gw.state.config().as_ref().clone().into(),
        model_registry: ModelRegistry::new(Default::default()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
        reloader: None,
    });
    let app = build_router(state);
    let (status, _, _) = call(&app, "POST", "/admin/reload", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&app, "GET", "/admin/reload", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_gateway_on_built_in_defaults_cannot_reload() {
    let config = GatewayConfig::load(None).ok();
    let _ = config;
    let reloader = Reloader::new(None, None, &GatewayConfig::from_file("config/gateway.yaml").unwrap());
    let state = Arc::new(AppState {
        config: GatewayConfig::from_file("config/gateway.yaml").unwrap().into(),
        model_registry: ModelRegistry::new(Default::default()),
        provider_registry: ProviderRegistry::new(),
        router: RoutingEngine::new("priority".to_string()),
        telemetry_tx: tokio::sync::mpsc::channel(1).0,
        start_time: Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        telemetry_health: Arc::new(ai_gateway::telemetry::TelemetryHealth::new(
            ai_gateway::telemetry::TelemetryMode::Disabled,
        )),
        response_cache: None,
        llama_server: None,
        analytics_pool: None,
        reloader: Some(Arc::new(reloader)),
    });
    let app = build_router(state);
    let (status, report) = reload(&app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(report["error"].as_str().unwrap().contains("built-in defaults"), "{report}");
}

// ─── File watcher ────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_watcher_reloads_when_the_file_changes_and_not_again_for_a_bad_version() {
    let (a_base, _a) = start_upstream().await;
    let (b_base, _b) = start_upstream().await;
    let gw = boot(&yaml(&[("a", &a_base, "")], &[("old", "a")], ""), None);

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let watcher = ai_gateway::reload::spawn_watcher_with_interval(gw.state.clone(), stop_rx, Duration::from_millis(80));

    // A valid edit is picked up on its own.
    gw.write_config(&yaml(&[("a", &a_base, ""), ("b", &b_base, "")], &[("old", "a"), ("new", "b")], ""));
    wait_for(|| gw.state.model_registry.has_alias("new")).await;
    assert_eq!(gw.state.reloader.as_ref().unwrap().status().reloads_applied, 1);
    assert_eq!(chat(&gw.app, "new", false).await.0, StatusCode::OK);

    // A broken edit is rejected once, the running config stays, and the same
    // bad file is not retried every poll.
    gw.write_config("providers: [broken");
    wait_for(|| gw.state.reloader.as_ref().unwrap().status().last.is_some_and(|l| !l.ok)).await;
    let first_failure_at = gw.state.reloader.as_ref().unwrap().status().last.unwrap().at;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        gw.state.reloader.as_ref().unwrap().status().last.unwrap().at,
        first_failure_at,
        "an unchanged bad file must not be reloaded again"
    );
    assert_eq!(chat(&gw.app, "new", false).await.0, StatusCode::OK, "still serving the last good config");

    // Fixing the file recovers.
    gw.write_config(&yaml(&[("a", &a_base, "")], &[("old", "a"), ("fixed", "a")], ""));
    wait_for(|| gw.state.model_registry.has_alias("fixed")).await;

    let _ = stop_tx.send(());
    watcher.await.unwrap();
    assert!(exists(&gw.config_path()));
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met within 10s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
