//! HTTP-level tests for `/llama-server/*`, sharing the fake-server harness in
//! `tests/llama_server_tests.rs` (this file is a submodule of it, so it is not
//! its own test target).

use super::*;
use std::sync::Arc;

use ai_gateway::config::GatewayConfig;
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{AppState, build_router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

fn state_with(manager: Option<LlamaServerManager>) -> Arc<AppState> {
    Arc::new(AppState {
        config: GatewayConfig::load(None).unwrap().into(),
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
        llama_server: manager.map(Arc::new),
        reloader: None,
        analytics_pool: None,
    })
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn poll_until_state(state: &Arc<AppState>, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, s) = call(state, "GET", "/llama-server/status", None, &[]).await;
        if s["state"] == want {
            return s;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {want}; last status: {s}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn disabled_feature_reports_status_gracefully_and_404s_everything_else() {
    let state = state_with(None);
    let (status, body) = call(&state, "GET", "/llama-server/status", None, &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], "disabled");

    for (method, path, b) in [
        ("GET", "/llama-server/models", None),
        ("GET", "/llama-server/logs", None),
        ("POST", "/llama-server/stop", None),
        ("POST", "/llama-server/start", Some(json!({"alias": "x"}))),
    ] {
        let (status, body) = call(&state, method, path, b, &[]).await;
        assert_eq!(status, 404, "{method} {path}");
        assert_eq!(body["error"]["code"], "feature_disabled");
    }
    // The pure parsing helper works regardless.
    let (status, body) =
        call(&state, "POST", "/llama-server/args/parse", Some(json!({"args": "-ngl all"})), &[]).await;
    assert_eq!((status, body["argv"].clone()), (200, json!(["-ngl", "all"])));
}

#[tokio::test]
async fn browser_origin_and_foreign_host_are_refused_on_every_route_but_plain_callers_pass() {
    let e = env().await;
    let state = state_with(Some(e.manager));

    let evil = [("origin", "https://evil.example")];
    for (method, path, body) in [
        ("GET", "/llama-server/status", None),
        ("GET", "/llama-server/models", None),
        ("GET", "/llama-server/logs", None),
        ("POST", "/llama-server/start", Some(json!({"alias": "x"}))),
        ("POST", "/llama-server/stop", None),
        ("POST", "/llama-server/models", Some(json!({"alias": "a", "gguf_path": "/x.gguf"}))),
        ("PUT", "/llama-server/defaults", Some(json!({"args": "-ngl 1"}))),
        ("POST", "/llama-server/args/parse", Some(json!({"args": ""}))),
    ] {
        let (status, resp) = call(&state, method, path, body, &evil).await;
        assert_eq!(status, 403, "{method} {path} must refuse a browser origin");
        assert_eq!(resp["error"]["code"], "origin_forbidden");
    }

    let (status, resp) =
        call(&state, "GET", "/llama-server/status", None, &[("sec-fetch-site", "cross-site")]).await;
    assert_eq!((status, resp["error"]["code"].clone()), (403, json!("origin_forbidden")));
    let (status, resp) =
        call(&state, "GET", "/llama-server/status", None, &[("host", "attacker.example:8999")]).await;
    assert_eq!((status, resp["error"]["code"].clone()), (403, json!("host_forbidden")));

    // Nothing was changed by the refused calls, and a plain caller passes.
    let (status, resp) =
        call(&state, "GET", "/llama-server/models", None, &[("host", "127.0.0.1:8999")]).await;
    assert_eq!(status, 200);
    assert_eq!(resp["models"], json!([]));
}

#[tokio::test]
async fn full_flow_save_start_inspect_switch_and_stop_over_http() {
    let e = env().await;
    let gguf_a = e.scratch.file("a.gguf");
    let gguf_b = e.scratch.file("b.gguf");
    let state = state_with(Some(e.manager));
    let lo = [("host", "127.0.0.1:8999")];

    // Validate, then create.
    let (_, v) = call(
        &state,
        "POST",
        "/llama-server/models/validate",
        Some(json!({"alias": "a", "gguf_path": gguf_a})),
        &lo,
    )
    .await;
    assert_eq!(v["valid"], true);
    let (_, v) = call(
        &state,
        "POST",
        "/llama-server/models/validate",
        Some(json!({"alias": "a", "gguf_path": "relative.gguf"})),
        &lo,
    )
    .await;
    assert_eq!((v["valid"].clone(), v["error"]["code"].clone()), (json!(false), json!("invalid_path")));

    let (status, a) = call(
        &state,
        "POST",
        "/llama-server/models",
        Some(json!({"alias": "model-a", "gguf_path": gguf_a, "args": "-ngl all --fake-load-ms 400"})),
        &lo,
    )
    .await;
    assert_eq!(status, 201, "{a}");
    let (status, b) = call(
        &state,
        "POST",
        "/llama-server/models",
        Some(json!({"alias": "model-b", "gguf_path": gguf_b})),
        &lo,
    )
    .await;
    assert_eq!(status, 201);

    let (status, dup) = call(
        &state,
        "POST",
        "/llama-server/models",
        Some(json!({"alias": "MODEL-A", "gguf_path": gguf_b})),
        &lo,
    )
    .await;
    assert_eq!((status, dup["error"]["code"].clone()), (409, json!("alias_taken")));
    let (status, bad) = call(
        &state,
        "POST",
        "/llama-server/models",
        Some(json!({"alias": "c", "gguf_path": gguf_b, "args": "--port 1"})),
        &lo,
    )
    .await;
    assert_eq!((status, bad["error"]["code"].clone()), (400, json!("reserved_flag")));

    let (_, list) = call(&state, "GET", "/llama-server/models", None, &lo).await;
    assert_eq!(list["models"].as_array().unwrap().len(), 2);
    assert_eq!(list["running_model_id"], Value::Null);

    // Start by alias -> 202, then poll to ready.
    let (status, started) =
        call(&state, "POST", "/llama-server/start", Some(json!({"alias": "model-a"})), &lo).await;
    assert_eq!(status, 202, "{started}");
    assert_eq!(started["managed"], true);
    let ready = poll_until_state(&state, "ready").await;
    assert_eq!(ready["model"]["alias"], "model-a");
    assert_eq!(ready["matched_model"]["alias"], "model-a");
    assert_eq!(ready["server"]["n_ctx"], 4096);
    assert!(ready["args"]["argv"].as_array().unwrap().iter().any(|v| v == "-ngl"));

    // A second start conflicts; delete-while-running conflicts.
    let (status, c) =
        call(&state, "POST", "/llama-server/start", Some(json!({"model_id": b["id"]})), &lo).await;
    assert_eq!((status, c["error"]["code"].clone()), (409, json!("already_running")));
    let a_path = format!("/llama-server/models/{}", a["id"].as_str().unwrap());
    let (status, c) = call(&state, "DELETE", &a_path, None, &lo).await;
    assert_eq!((status, c["error"]["code"].clone()), (409, json!("model_in_use")));
    let (_, list) = call(&state, "GET", "/llama-server/models", None, &lo).await;
    assert_eq!(list["running_model_id"], a["id"]);

    // Logs over HTTP with a cursor.
    let (_, logs) = call(&state, "GET", "/llama-server/logs?limit=1000", None, &lo).await;
    assert!(logs["lines"].as_array().unwrap().iter().any(|l| l["text"].as_str().unwrap().contains("starting")));
    let cursor = logs["next_cursor"].as_u64().unwrap();
    let (_, more) = call(&state, "GET", &format!("/llama-server/logs?after={cursor}"), None, &lo).await;
    assert!(more["lines"].as_array().unwrap().is_empty());

    // Switch models with replace + save the args used.
    let (status, sw) = call(
        &state,
        "POST",
        "/llama-server/start",
        Some(json!({"model_id": b["id"], "replace": true, "args": "-np 2", "save_args": true})),
        &lo,
    )
    .await;
    assert_eq!(status, 202, "{sw}");
    let ready = poll_until_state(&state, "ready").await;
    assert_eq!(ready["model"]["alias"], "model-b");
    assert_eq!(ready["server"]["total_slots"], 2);
    let (_, list) = call(&state, "GET", "/llama-server/models", None, &lo).await;
    let saved_b = list["models"].as_array().unwrap().iter().find(|m| m["alias"] == "model-b").unwrap();
    assert_eq!(saved_b["args"], "-np 2", "save_args persisted the args onto the model");

    // Stop, then editing and deleting work.
    let (status, stopped) = call(&state, "POST", "/llama-server/stop", None, &lo).await;
    assert_eq!((status, stopped["state"].clone()), (200, json!("stopped")));
    let (status, upd) = call(
        &state,
        "PUT",
        &a_path,
        Some(json!({"alias": "renamed", "gguf_path": gguf_a})),
        &lo,
    )
    .await;
    assert_eq!((status, upd["alias"].clone(), upd["args"].clone()), (200, json!("renamed"), Value::Null));
    let (status, _) = call(&state, "DELETE", &a_path, None, &lo).await;
    assert_eq!(status, 204);
    let (status, nf) = call(&state, "DELETE", &a_path, None, &lo).await;
    assert_eq!((status, nf["error"]["code"].clone()), (404, json!("not_found")));
}

#[tokio::test]
async fn defaults_and_arg_parsing_endpoints() {
    let e = env().await;
    let state = state_with(Some(e.manager));
    let (status, put) =
        call(&state, "PUT", "/llama-server/defaults", Some(json!({"args": "-ngl all -fa on"})), &[]).await;
    assert_eq!((status, put["args"].clone()), (200, json!("-ngl all -fa on")));
    let (_, get) = call(&state, "GET", "/llama-server/defaults", None, &[]).await;
    assert_eq!(get["args"], "-ngl all -fa on");
    let (status, bad) =
        call(&state, "PUT", "/llama-server/defaults", Some(json!({"args": "--host 0.0.0.0"})), &[]).await;
    assert_eq!((status, bad["error"]["code"].clone()), (400, json!("reserved_flag")));

    let (_, parsed) =
        call(&state, "POST", "/llama-server/args/parse", Some(json!({"args": "-ngl all 'x"})), &[]).await;
    assert_eq!(parsed["issues"][0]["code"], "unbalanced_quote");
    let (_, parsed) =
        call(&state, "POST", "/llama-server/args/parse", Some(json!({"args": "-ngl all --port 9"})), &[]).await;
    assert_eq!(parsed["issues"][0]["code"], "reserved_flag");
    assert_eq!(parsed["issues"][0]["token"], "--port");
}

#[tokio::test]
async fn start_requires_exactly_one_selector_and_unknown_models_404() {
    let e = env().await;
    let state = state_with(Some(e.manager));
    let (status, r) = call(&state, "POST", "/llama-server/start", Some(json!({})), &[]).await;
    assert_eq!((status, r["error"]["code"].clone()), (400, json!("invalid_request")));
    let (status, r) = call(
        &state,
        "POST",
        "/llama-server/start",
        Some(json!({"alias": "x", "model_id": Uuid::now_v7()})),
        &[],
    )
    .await;
    assert_eq!((status, r["error"]["code"].clone()), (400, json!("invalid_request")));
    let (status, r) = call(&state, "POST", "/llama-server/start", Some(json!({"alias": "ghost"})), &[]).await;
    assert_eq!((status, r["error"]["code"].clone()), (404, json!("not_found")));
}

#[tokio::test]
async fn scan_endpoint_reports_configured_state() {
    let root = Scratch::new();
    root.file("m.gguf");
    let dir = root.0.to_string_lossy().into_owned();
    let e = env_with(move |c| c.scan_dirs = vec![dir]).await;
    let state = state_with(Some(e.manager));
    let (status, r) = call(&state, "GET", "/llama-server/models/scan", None, &[]).await;
    assert_eq!(status, 200);
    assert_eq!(r["configured"], true);
    assert_eq!(r["dirs"][0]["models"][0]["file_name"], "m.gguf");

    let e2 = env().await;
    let state2 = state_with(Some(e2.manager));
    let (_, r) = call(&state2, "GET", "/llama-server/models/scan", None, &[]).await;
    assert_eq!((r["configured"].clone(), r["dirs"].clone()), (json!(false), json!([])));
}
