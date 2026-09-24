-- `provider_attempts.timestamp` is set once, at ProviderAttemptStarted, and
-- never renamed to `started_at` in the schema even though that's what it
-- actually is. There was no column at all for when an attempt finished —
-- only `duration_us`, which the analytics API had to leave the caller to
-- add back to `timestamp` themselves. This adds the column directly so
-- request-detail responses can expose `started_at`/`finished_at` without
-- forcing every consumer to redo that arithmetic.
-- See docs/frontend/API_GAPS.md item 5.

ALTER TABLE provider_attempts ADD COLUMN finished_at TIMESTAMPTZ;
