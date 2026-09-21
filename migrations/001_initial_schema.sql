-- AI Gateway — Initial Database Schema
-- ══════════════════════════════════════════════════════════════════════════════
--
-- Normalized relational tables — NOT one giant "requests" table.
--
-- Design principle: Store observations; calculate summaries.
-- Never permanently reduce raw data into aggregates.
--
-- This schema supports the full data lifecycle:
--   RAW → NORMALIZED → DERIVED → CURATED → SFT / DPO / eval sets

-- ─── Extensions ──────────────────────────────────────────────────────────────

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- ─── inference_requests ──────────────────────────────────────────────────────
-- Top-level request record. One row per gateway request.

CREATE TABLE inference_requests (
    request_id          UUID PRIMARY KEY,
    received_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at        TIMESTAMPTZ,

    -- Application / task metadata
    application         TEXT,
    task                TEXT,

    -- Model identity
    requested_model     TEXT NOT NULL,
    resolved_model      TEXT,
    serving_provider    TEXT,

    -- Request characteristics
    streaming           BOOLEAN NOT NULL DEFAULT FALSE,
    uses_tools          BOOLEAN NOT NULL DEFAULT FALSE,
    uses_vision         BOOLEAN NOT NULL DEFAULT FALSE,
    uses_structured_output BOOLEAN NOT NULL DEFAULT FALSE,
    uses_reasoning      BOOLEAN NOT NULL DEFAULT FALSE,

    -- Outcome
    success             BOOLEAN,
    total_latency_us    BIGINT,
    total_attempts      INTEGER DEFAULT 0,

    -- Full input messages (for dataset reconstruction)
    canonical_messages  JSONB,

    -- Raw preservation
    raw_request         JSONB,

    -- Timestamps
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_inference_requests_received_at ON inference_requests (received_at);
CREATE INDEX idx_inference_requests_model ON inference_requests (requested_model);
CREATE INDEX idx_inference_requests_provider ON inference_requests (serving_provider);
CREATE INDEX idx_inference_requests_application ON inference_requests (application);

-- ─── request_tags ────────────────────────────────────────────────────────────
-- User-defined tags for filtering and analysis.

CREATE TABLE request_tags (
    id              BIGSERIAL PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES inference_requests(request_id),
    tag             TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_request_tags_request_id ON request_tags (request_id);
CREATE INDEX idx_request_tags_tag ON request_tags (tag);

-- ─── routing_decisions ───────────────────────────────────────────────────────
-- Full routing reasoning. "Don't just store the final destination.
-- Store why it became the destination."

CREATE TABLE routing_decisions (
    id                      BIGSERIAL PRIMARY KEY,
    request_id              UUID NOT NULL REFERENCES inference_requests(request_id),
    timestamp               TIMESTAMPTZ NOT NULL,

    requested_model         TEXT NOT NULL,
    resolution_type         TEXT NOT NULL,  -- 'alias', 'explicit_model', 'explicit_provider'

    candidates_considered   INTEGER NOT NULL DEFAULT 0,
    candidates_rejected     INTEGER NOT NULL DEFAULT 0,
    rejection_reasons       JSONB,  -- Array of {provider, model, reason}

    selected_provider       TEXT,
    selected_model          TEXT,
    policy_applied          TEXT NOT NULL,
    short_circuit           TEXT,  -- 'explicit_provider', 'explicit_model', or NULL

    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_routing_decisions_request_id ON routing_decisions (request_id);

-- ─── provider_attempts ───────────────────────────────────────────────────────
-- Each attempt in the fallback chain. Stores the full attempt hierarchy.
--
-- Example:
--   attempt #1: llama-server → failed (connection_refused, 42ms)
--   attempt #2: openrouter → success (DeepInfra upstream, TTFT 382ms)

CREATE TABLE provider_attempts (
    id                      BIGSERIAL PRIMARY KEY,
    request_id              UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number          INTEGER NOT NULL,
    timestamp               TIMESTAMPTZ NOT NULL,

    provider                TEXT NOT NULL,
    model                   TEXT NOT NULL,

    -- Outcome
    status                  TEXT NOT NULL,  -- 'success', 'failed', 'timeout', 'rate_limited', 'cancelled'
    finish_reason           TEXT,
    duration_us             BIGINT,

    -- Upstream provider (e.g. DeepInfra behind OpenRouter)
    upstream_provider       TEXT,
    provider_response_id    TEXT,

    -- Error details
    error_code              TEXT,
    error_message           TEXT,

    -- Connection timing
    connection_latency_us   BIGINT,

    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_provider_attempts_request_id ON provider_attempts (request_id);
CREATE INDEX idx_provider_attempts_provider ON provider_attempts (provider);
CREATE INDEX idx_provider_attempts_status ON provider_attempts (status);

-- ─── generations ─────────────────────────────────────────────────────────────
-- The completed generation output per attempt.
-- Includes FULL generated text — critical for fine-tuning/dataset goal.

CREATE TABLE generations (
    id                  BIGSERIAL PRIMARY KEY,
    request_id          UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number      INTEGER NOT NULL,
    timestamp           TIMESTAMPTZ NOT NULL,

    -- Full generated text content (for dataset reconstruction)
    content             TEXT,

    -- Full reasoning content (if exposed by provider)
    reasoning_content   TEXT,

    -- Reasoning metadata
    reasoning_visibility TEXT,  -- 'exposed', 'summary_only', 'count_only', 'unavailable', 'none'
    reasoning_summary   TEXT,
    reasoning_token_count BIGINT,

    -- Tool calls (normalized)
    tool_calls          JSONB,

    -- Raw preservation
    raw_response        JSONB,

    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_generations_request_id ON generations (request_id);

-- ─── generation_usage ────────────────────────────────────────────────────────
-- Token counts per attempt.

CREATE TABLE generation_usage (
    id                  BIGSERIAL PRIMARY KEY,
    request_id          UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number      INTEGER NOT NULL,
    timestamp           TIMESTAMPTZ NOT NULL,

    prompt_tokens       BIGINT NOT NULL DEFAULT 0,
    completion_tokens   BIGINT NOT NULL DEFAULT 0,
    total_tokens        BIGINT NOT NULL DEFAULT 0,
    cached_tokens       BIGINT,
    reasoning_tokens    BIGINT,

    -- Raw provider usage (preserved for analysis)
    raw_provider_usage  JSONB,

    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_generation_usage_request_id ON generation_usage (request_id);

-- ─── stream_events ───────────────────────────────────────────────────────────
-- Per-chunk timestamps for full timeline reconstruction.
--
-- Enables deriving: routing_latency, connection_latency, TTFB, TTFT,
-- prefill_estimate, generation_duration, inter_chunk_latency,
-- stream_duration, output_throughput, end-to-end_throughput,
-- provider_overhead, gateway_overhead
--
-- "Store observations; calculate summaries."
-- [37ms, 31ms, 42ms, 29ms, 116ms, ...] NOT avg_chunk_latency = 43.2ms

CREATE TABLE stream_events (
    id              BIGSERIAL PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number  INTEGER NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    sequence        BIGINT NOT NULL,

    chunk_type      TEXT NOT NULL,  -- 'content', 'reasoning', 'tool_call', 'usage', etc.
    byte_count      INTEGER DEFAULT 0,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_stream_events_request_id ON stream_events (request_id);
CREATE INDEX idx_stream_events_timestamp ON stream_events (timestamp);

-- ─── tool_calls ──────────────────────────────────────────────────────────────
-- Extracted, normalized tool call records.

CREATE TABLE tool_calls (
    id              BIGSERIAL PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number  INTEGER NOT NULL,

    tool_call_id    TEXT,
    function_name   TEXT NOT NULL,
    arguments       TEXT,  -- JSON string of arguments
    result          TEXT,  -- Tool result (if this is a tool response)

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tool_calls_request_id ON tool_calls (request_id);
CREATE INDEX idx_tool_calls_function ON tool_calls (function_name);

-- ─── errors ──────────────────────────────────────────────────────────────────
-- Structured error records.

CREATE TABLE errors (
    id              BIGSERIAL PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES inference_requests(request_id),
    attempt_number  INTEGER,
    timestamp       TIMESTAMPTZ NOT NULL,

    error_code      TEXT NOT NULL,
    error_message   TEXT,
    status_code     INTEGER,
    provider        TEXT,
    retryable       BOOLEAN DEFAULT FALSE,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_errors_request_id ON errors (request_id);
CREATE INDEX idx_errors_code ON errors (error_code);

-- ─── cost_records ────────────────────────────────────────────────────────────
-- Estimated and actual reported costs.

CREATE TABLE cost_records (
    id              BIGSERIAL PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES inference_requests(request_id),
    timestamp       TIMESTAMPTZ NOT NULL,

    estimated_cost  DOUBLE PRECISION,
    actual_cost     DOUBLE PRECISION,
    currency        TEXT NOT NULL DEFAULT 'USD',

    provider        TEXT,
    model           TEXT,

    -- Cost breakdown (if available)
    input_cost      DOUBLE PRECISION,
    output_cost     DOUBLE PRECISION,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_cost_records_request_id ON cost_records (request_id);

-- ─── model_snapshots ─────────────────────────────────────────────────────────
-- Point-in-time model metadata.

CREATE TABLE model_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    snapshot_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    model_name      TEXT NOT NULL,
    providers       JSONB,
    capabilities    JSONB,
    config          JSONB,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ─── provider_snapshots ──────────────────────────────────────────────────────
-- Point-in-time provider health/config.

CREATE TABLE provider_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    snapshot_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    provider_name   TEXT NOT NULL,
    provider_type   TEXT NOT NULL,
    health          TEXT NOT NULL,
    config          JSONB,

    total_requests  BIGINT DEFAULT 0,
    total_errors    BIGINT DEFAULT 0,
    avg_latency_ms  DOUBLE PRECISION,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_provider_snapshots_provider ON provider_snapshots (provider_name);
CREATE INDEX idx_provider_snapshots_time ON provider_snapshots (snapshot_at);
