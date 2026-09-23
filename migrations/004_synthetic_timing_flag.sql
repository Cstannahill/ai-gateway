-- Distinguishes timing derived from a genuine live provider stream from
-- timing fabricated for a non-streaming response reconstructed into events
-- after the fact (all such events land within microseconds of each other,
-- since the whole response arrived in one HTTP round-trip). Analytical
-- TTFB/TTFT/inter-chunk-latency queries must exclude these rows/attempts —
-- averaging them in would silently understate real streaming latency.
-- See docs/REMEDIATION_PLAN.md Phase 3.1 / Audit A06.

ALTER TABLE stream_events ADD COLUMN synthetic BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE provider_attempts ADD COLUMN synthetic_timing BOOLEAN NOT NULL DEFAULT false;
