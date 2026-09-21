use axum::http::Request;
use tower_http::request_id::{MakeRequestId, RequestId};
use uuid::Uuid;

// ─── Middleware ───────────────────────────────────────────────────────────────

/// Generate UUIDv7 request IDs for every incoming request.
#[derive(Clone)]
pub struct UuidV7RequestId;

impl MakeRequestId for UuidV7RequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        Some(RequestId::new(id.parse().ok()?))
    }
}
