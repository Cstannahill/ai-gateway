//! The application icon, served so a browser tab pointed at the gateway (its
//! URL, `/health`, `/providers`, ...) shows the AI Gateway logo instead of
//! requesting a favicon that 404s. Generated from `AI-Gateway.png` by
//! `scripts/generate-icons.mjs`; embedded in the binary.

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::IntoResponse;

static FAVICON_ICO: &[u8] = include_bytes!("../../../assets/favicon.ico");
static ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

/// Icons change only with a release, so a day of caching is plenty.
const CACHE: &str = "public, max-age=86400";

/// GET /favicon.ico
pub async fn favicon() -> impl IntoResponse {
    ([(CONTENT_TYPE, "image/x-icon"), (CACHE_CONTROL, CACHE)], FAVICON_ICO)
}

/// GET /icon.png (256x256)
pub async fn icon_png() -> impl IntoResponse {
    ([(CONTENT_TYPE, "image/png"), (CACHE_CONTROL, CACHE)], ICON_PNG)
}
