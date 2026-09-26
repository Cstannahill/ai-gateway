// ─── /llama-server/* ─────────────────────────────────────────────────────────
//
// HTTP surface for the llama-server manager (`crate::llama_server`). The full
// contract, with example payloads, is in `docs/frontend/LLAMA_SERVER_UI.md`.
//
// SECURITY: these routes can execute a local binary, and the gateway's CORS
// layer is permissive, so any web page open in the user's browser could
// otherwise call them. `browser_guard` (applied to every route here, reads
// included) refuses requests that come from a browser page or from an
// unexpected Host header. Callers that are not browsers — the frontend's
// server-side proxy, curl — send neither an `Origin` nor a cross-site
// `Sec-Fetch-Site`, and pass.

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::config::is_loopback_host;
use crate::llama_server::store::{ModelInput, ModelStore};
use crate::llama_server::{LlamaError, LlamaServerManager, args};
use crate::server::AppState;

// ─── Errors ──────────────────────────────────────────────────────────────────

fn error_body(code: &str, message: &str) -> serde_json::Value {
    json!({ "error": { "message": message, "type": code, "code": code } })
}

impl IntoResponse for LlamaError {
    fn into_response(self) -> Response {
        let status = match &self {
            LlamaError::Disabled | LlamaError::NotFound(_) => StatusCode::NOT_FOUND,
            LlamaError::Invalid { .. } => StatusCode::BAD_REQUEST,
            LlamaError::Conflict { .. } => StatusCode::CONFLICT,
            LlamaError::Failed { .. } | LlamaError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(error_body(self.code(), &self.message()))).into_response()
    }
}

fn manager(state: &AppState) -> Result<&Arc<LlamaServerManager>, LlamaError> {
    state.llama_server.as_ref().ok_or(LlamaError::Disabled)
}

// ─── Browser guard ───────────────────────────────────────────────────────────

fn forbidden(code: &str, message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(error_body(code, message))).into_response()
}

/// The host part of a `Host` header value (port and IPv6 brackets removed).
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

/// Decide whether a request may reach the llama-server routes. Pure so it can
/// be unit-tested exhaustively.
///
/// * A browser page sends `Origin` (any cross-origin `fetch`/XHR/form POST) —
///   refused unless allow-listed.
/// * `Sec-Fetch-Site: cross-site|same-site` catches the cases with no `Origin`
///   (e.g. a cross-site `<img>`/navigation GET).
/// * A `Host` that isn't loopback defeats DNS-rebinding (an attacker's domain
///   that resolves to 127.0.0.1 still sends its own name as Host).
pub(crate) fn check_browser_guard(
    headers: &HeaderMap,
    allowed_origins: &[String],
    allow_non_loopback: bool,
) -> Result<(), (&'static str, String)> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    if let Some(origin) = get("origin") {
        let allowed = allowed_origins.iter().any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(origin.trim_end_matches('/')));
        if !allowed {
            return Err((
                "origin_forbidden",
                format!("requests from the browser origin '{origin}' are not allowed; use the frontend's server-side proxy, or add the origin to llama_server.allowed_origins"),
            ));
        }
    } else if let Some(site) = get("sec-fetch-site")
        && matches!(site, "cross-site" | "same-site")
    {
        return Err((
            "origin_forbidden",
            format!("cross-site browser requests (Sec-Fetch-Site: {site}) are not allowed"),
        ));
    }

    if !allow_non_loopback
        && let Some(host) = get("host")
    {
        let h = host_without_port(host);
        if !is_loopback_host(h) {
            return Err((
                "host_forbidden",
                format!("Host '{host}' is not a loopback address; this endpoint is only reachable via localhost"),
            ));
        }
    }
    Ok(())
}

pub async fn browser_guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let (allowed, allow_non_loopback) = match &state.llama_server {
        Some(m) => (m.config().allowed_origins.clone(), m.config().allow_non_loopback),
        None => (vec![], false),
    };
    match check_browser_guard(request.headers(), &allowed, allow_non_loopback) {
        Ok(()) => next.run(request).await,
        Err((code, message)) => forbidden(code, &message),
    }
}

// ─── Status / control ────────────────────────────────────────────────────────

/// `GET /llama-server/status`. Answers `200` even when the feature is off so a
/// UI can render a "not enabled" state without treating it as an error.
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    match &state.llama_server {
        Some(m) => Json(m.status().await).into_response(),
        None => Json(json!({
            "enabled": false,
            "state": "disabled",
            "message": LlamaError::Disabled.message(),
        }))
        .into_response(),
    }
}

#[derive(Deserialize)]
pub struct StartRequest {
    /// Saved model id. Exactly one of `model_id` / `alias` is required.
    #[serde(default)]
    model_id: Option<Uuid>,
    #[serde(default)]
    alias: Option<String>,
    /// Argument string for this run only. Omit to use the model's saved args
    /// (or the global defaults).
    #[serde(default)]
    args: Option<String>,
    /// Stop a running server first (switch models).
    #[serde(default)]
    replace: bool,
    /// Also save `args` onto the model.
    #[serde(default)]
    save_args: bool,
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StartRequest>,
) -> Result<(StatusCode, Json<crate::llama_server::StatusSnapshot>), LlamaError> {
    let m = manager(&state)?;
    let id = match (req.model_id, req.alias.as_deref()) {
        (Some(id), None) => id,
        (None, Some(alias)) => m
            .store()
            .list()
            .await
            .into_iter()
            .find(|s| s.alias.eq_ignore_ascii_case(alias))
            .map(|s| s.id)
            .ok_or_else(|| LlamaError::NotFound(format!("no saved model named '{alias}'")))?,
        _ => {
            return Err(LlamaError::invalid(
                "invalid_request",
                "provide exactly one of model_id or alias".to_string(),
            ));
        }
    };

    if req.save_args {
        let Some(args) = &req.args else {
            return Err(LlamaError::invalid("invalid_request", "save_args requires args".to_string()));
        };
        let model = m
            .store()
            .get(id)
            .await
            .ok_or_else(|| LlamaError::NotFound(format!("no saved model with id {id}")))?;
        m.store()
            .update(
                id,
                ModelInput {
                    alias: model.alias,
                    gguf_path: model.gguf_path,
                    mmproj_path: model.mmproj_path,
                    args: Some(args.clone()),
                    pass_alias: Some(model.pass_alias),
                    notes: model.notes,
                },
            )
            .await?;
    }

    let snapshot = m.start(id, req.args, req.replace).await?;
    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

pub async fn stop(State(state): State<Arc<AppState>>) -> Result<Json<crate::llama_server::StatusSnapshot>, LlamaError> {
    Ok(Json(manager(&state)?.stop().await))
}

pub async fn restart(
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<crate::llama_server::StatusSnapshot>), LlamaError> {
    Ok((StatusCode::ACCEPTED, Json(manager(&state)?.restart().await?)))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

pub async fn logs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<serde_json::Value>, LlamaError> {
    let m = manager(&state)?;
    let page = m.logs(q.after, q.limit.unwrap_or(500).clamp(1, 2000));
    Ok(Json(json!({
        "lines": page.lines,
        "next_cursor": page.next_cursor,
        "truncated": page.truncated,
        "state": m.state(),
    })))
}

// ─── Saved models ────────────────────────────────────────────────────────────

pub async fn list_models(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, LlamaError> {
    let m = manager(&state)?;
    let models = m.store().list().await;
    Ok(Json(json!({
        "models": models,
        "default_args": m.store().default_args().await,
        "running_model_id": m.running_model_id(),
    })))
}

pub async fn create_model(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ModelInput>,
) -> Result<(StatusCode, Json<crate::llama_server::store::SavedModel>), LlamaError> {
    Ok((StatusCode::CREATED, Json(manager(&state)?.store().create(input).await?)))
}

pub async fn update_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(input): Json<ModelInput>,
) -> Result<Json<crate::llama_server::store::SavedModel>, LlamaError> {
    Ok(Json(manager(&state)?.store().update(id, input).await?))
}

pub async fn delete_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, LlamaError> {
    let m = manager(&state)?;
    if m.running_model_id() == Some(id) {
        return Err(LlamaError::conflict(
            "model_in_use",
            "this model is currently running; stop the server before deleting it".to_string(),
        ));
    }
    m.store().delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /llama-server/models/validate` — check a would-be model without
/// saving it. Always `200`; `valid: false` carries the first problem so a form
/// can show it inline.
pub async fn validate_model(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ModelInput>,
) -> Result<Json<serde_json::Value>, LlamaError> {
    let m = manager(&state)?;
    let taken = m
        .store()
        .list()
        .await
        .iter()
        .any(|s| s.alias.eq_ignore_ascii_case(&input.alias));
    Ok(Json(match ModelStore::validate_input(&input) {
        Ok((gguf, mmproj)) => json!({
            "valid": true,
            "gguf_path": gguf,
            "mmproj_path": mmproj,
            // Informational: creating would conflict, but editing the model
            // that owns this alias is fine.
            "alias_taken": taken,
        }),
        Err(e) => json!({
            "valid": false,
            "error": { "code": e.code(), "message": e.message() },
        }),
    }))
}

pub async fn scan_models(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, LlamaError> {
    let m = manager(&state)?;
    Ok(Json(json!({
        "configured": !m.config().scan_dirs.is_empty(),
        "dirs": m.scan().await,
    })))
}

// ─── Arguments ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ArgsBody {
    args: String,
}

pub async fn get_defaults(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, LlamaError> {
    Ok(Json(json!({ "args": manager(&state)?.store().default_args().await })))
}

pub async fn set_defaults(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ArgsBody>,
) -> Result<Json<serde_json::Value>, LlamaError> {
    let saved = manager(&state)?.store().set_default_args(body.args).await?;
    Ok(Json(json!({ "args": saved })))
}

/// `POST /llama-server/args/parse` — live tokenization preview for the UI.
/// Always `200`; problems are in `issues`. Does not require the feature to be
/// enabled (it is pure).
pub async fn parse_args(Json(body): Json<ArgsBody>) -> Json<args::ParsedArgs> {
    Json(args::parse_and_validate(&body.args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn check(pairs: &[(&'static str, &str)]) -> Result<(), &'static str> {
        check_browser_guard(&headers(pairs), &[], false).map_err(|(code, _)| code)
    }

    #[test]
    fn non_browser_callers_pass() {
        assert!(check(&[]).is_ok(), "no headers at all (curl / server-side proxy)");
        assert!(check(&[("host", "127.0.0.1:8999")]).is_ok());
        assert!(check(&[("host", "localhost:8999")]).is_ok());
        assert!(check(&[("host", "[::1]:8999")]).is_ok());
        assert!(check(&[("sec-fetch-site", "none"), ("host", "localhost")]).is_ok(), "typed into the address bar");
        assert!(check(&[("sec-fetch-site", "same-origin")]).is_ok());
    }

    #[test]
    fn any_browser_origin_is_refused_by_default() {
        assert_eq!(check(&[("origin", "https://evil.example")]), Err("origin_forbidden"));
        assert_eq!(check(&[("origin", "http://localhost:3000")]), Err("origin_forbidden"), "even the local frontend must go through its server-side proxy");
        assert_eq!(check(&[("origin", "null")]), Err("origin_forbidden"));
    }

    #[test]
    fn allow_listed_origins_pass_case_and_slash_insensitively() {
        let allowed = vec!["http://localhost:3000".to_string()];
        assert!(check_browser_guard(&headers(&[("origin", "http://LOCALHOST:3000/")]), &allowed, false).is_ok());
        assert!(check_browser_guard(&headers(&[("origin", "http://localhost:3001")]), &allowed, false).is_err());
    }

    #[test]
    fn cross_site_fetches_without_an_origin_header_are_refused() {
        assert_eq!(check(&[("sec-fetch-site", "cross-site")]), Err("origin_forbidden"));
        assert_eq!(check(&[("sec-fetch-site", "same-site")]), Err("origin_forbidden"));
    }

    #[test]
    fn non_loopback_hosts_are_refused_to_defeat_dns_rebinding() {
        assert_eq!(check(&[("host", "attacker.example:8999")]), Err("host_forbidden"));
        assert_eq!(check(&[("host", "192.168.1.5:8999")]), Err("host_forbidden"));
        assert!(check_browser_guard(&headers(&[("host", "192.168.1.5:8999")]), &[], true).is_ok(), "allow_non_loopback opts out");
    }

    #[test]
    fn host_port_stripping() {
        assert_eq!(host_without_port("localhost:8999"), "localhost");
        assert_eq!(host_without_port("[::1]:8999"), "::1");
        assert_eq!(host_without_port("127.0.0.1"), "127.0.0.1");
    }
}
