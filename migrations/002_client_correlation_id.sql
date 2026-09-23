-- AI Gateway — client_correlation_id
-- ══════════════════════════════════════════════════════════════════════════════
--
-- Decouples the client-suppliable `x-request-id` header from the database
-- primary key. Previously an inbound header value was used directly as
-- `inference_requests.request_id`, and RequestReceived's upsert meant a
-- client reusing that header across two distinct inferences would silently
-- overwrite the first request's stored model/messages/raw body.
--
-- `request_id` remains the primary key, but is now always gateway-generated
-- (UUIDv7). The client-supplied value (if any) is preserved separately here
-- for tracing/idempotency purposes only — it is never used as a join key
-- with upsert semantics.
--
-- See docs/REMEDIATION_PLAN.md Phase 0.4 / docs/INIT_CONVERSATION_AUDIT.md A15.

ALTER TABLE inference_requests ADD COLUMN client_correlation_id TEXT;

CREATE INDEX idx_inference_requests_client_correlation_id
    ON inference_requests (client_correlation_id)
    WHERE client_correlation_id IS NOT NULL;
