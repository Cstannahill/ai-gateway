-- AI Gateway — first_byte_at / first_token_at
-- ══════════════════════════════════════════════════════════════════════════════
--
-- Adds columns for two of the conversation's named timeline events
-- (INIT_CONVERSATION.md's "first provider byte" / "first generation chunk")
-- that had no persistence path at all — `TelemetryEvent::FirstProviderByte`
-- and `FirstGenerationChunk` were previously received by the writer and
-- silently discarded as no-ops.
--
-- See docs/REMEDIATION_PLAN.md Phase 3.1 / docs/INIT_CONVERSATION_AUDIT.md A06.

ALTER TABLE provider_attempts ADD COLUMN first_byte_at TIMESTAMPTZ;
ALTER TABLE provider_attempts ADD COLUMN first_token_at TIMESTAMPTZ;
