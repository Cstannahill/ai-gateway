//! `/admin/*` - operator controls for the running gateway.
//!
//! Currently: configuration hot-reload (`POST /admin/reload`, status via
//! `GET /admin/reload`). These routes change what the gateway does, and CORS is
//! permissive, so - like `/llama-server/*` - they refuse requests from a
//! browser page (`Origin` / cross-site `Sec-Fetch-Site`) and from a non-loopback
//! `Host` (DNS rebinding). The dashboard's server-side proxy and `curl` send
//! neither and pass.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::server::AppState;
use crate::server::routes::llama_server::check_browser_guard;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "message": message, "type": code, "code": code } })),
    )
        .into_response()
}

pub async fn admin_guard(request: Request, next: Next) -> Response {
    match check_browser_guard(request.headers(), &[], false) {
        Ok(()) => next.run(request).await,
        Err((code, message)) => error(StatusCode::FORBIDDEN, code, &message),
    }
}

/// `POST /admin/reload` - re-read `gateway.yaml` and the env file and apply
/// what can be applied. `200` with the report on success (including "no
/// changes"); `422` with the same report shape when the new configuration was
/// rejected - the running configuration is then unchanged.
pub async fn reload(State(state): State<Arc<AppState>>) -> Response {
    let Some(reloader) = state.reloader.clone() else {
        return error(
            StatusCode::NOT_FOUND,
            "reload_unavailable",
            "this gateway instance was started without config reloading",
        );
    };
    let report = reloader.reload(&state, "endpoint").await;
    crate::reload::log_report(&report);
    let status = if report.ok { StatusCode::OK } else { StatusCode::UNPROCESSABLE_ENTITY };
    (status, Json(report)).into_response()
}

/// `GET /admin/reload` - the files being watched, how many reloads have been
/// applied, and the outcome of the most recent attempt.
pub async fn reload_status(State(state): State<Arc<AppState>>) -> Response {
    match &state.reloader {
        Some(reloader) => Json(reloader.status()).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "reload_unavailable",
            "this gateway instance was started without config reloading",
        ),
    }
}
