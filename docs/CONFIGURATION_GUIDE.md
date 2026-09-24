# Configuration Guide

This document is the complete reference for configuring the **AI Gateway** via [`config/gateway.yaml`](file:///s:/Code/RustProjects/AI-Gateway/config/gateway.yaml).

---

## 1. Configuration Loading Hierarchy

When the gateway binary starts, it resolves configuration in the following order:

1. **CLI Argument**: Path passed as the first CLI parameter:
   ```bash
   ai-gateway /path/to/my-config.yaml
   ```
2. **Standard File**: `config/gateway.yaml` in current working directory.
3. **Root File**: `gateway.yaml` in current working directory.
4. **Internal Defaults**: If no file is discovered, the gateway initializes default in-memory configurations and emits a warning log.

---

## 2. Complete Configuration Reference

Below is a complete, annotated example showing all available fields across all sections.

```yaml
# ══════════════════════════════════════════════════════════════════════════════
# AI Gateway Configuration Reference
# ══════════════════════════════════════════════════════════════════════════════

# ─── 1. HTTP Server Settings ─────────────────────────────────────────────────
server:
  # Network interface to bind to (e.g. "0.0.0.0" for all interfaces or "127.0.0.1" for localhost)
  host: "0.0.0.0"

  # Port to listen on
  port: 8999

  # Maximum inbound HTTP body size in bytes (default: 52428800 = 50 MB)
  # Protects against memory exhaustion from oversized multimodal base64 payloads
  body_limit_bytes: 52428800

  # Graceful shutdown timeout in seconds. Active connections are allowed this much time to finish
  shutdown_timeout_secs: 30


# ─── 2. Provider Adapters ───────────────────────────────────────────────────
# Define each provider backend available to the gateway.
providers:
  # Unique provider key used in candidates and explicit addressing
  llama:
    name: "llama"
    # Supported types: llama_cpp, open_router, ollama, ollama_cloud, vllm, gemini, hugging_face, open_ai_compatible
    type: "llama_cpp"
    # Base URL for the provider's API endpoint (without trailing slash)
    base_url: "http://localhost:8080"
    # Per-request timeout in seconds for this provider
    timeout_secs: 300
    # Enable or disable this provider without deleting configuration
    enabled: true
    # Optional maximum concurrent in-flight requests for this provider.
    # Enforced at routing time: a candidate at or over this limit is
    # filtered out with rejection reason "provider_at_capacity" (distinct
    # from "provider_unavailable") and the router moves to the next
    # candidate, same as an unhealthy/disabled provider — including for
    # explicit `provider/model` addressing, which does not bypass it (it's
    # a resource-protection constraint, not a routing preference). Unset
    # means no limit.
    # max_concurrent: 4
    # Optional circuit breaker: after this many *consecutive* attempt
    # failures (tracked by ProviderRegistry::record_request_end, reset on
    # any success), routing filters this candidate out with rejection
    # reason "provider_circuit_open" — again including explicit addressing,
    # for the same resource-protection rationale as max_concurrent. Unset
    # (the default) disables the breaker entirely; a flaky provider is then
    # only ever skipped via the existing per-attempt fallback/retry logic,
    # not pre-emptively across requests.
    # circuit_breaker_threshold: 5
    # Seconds the circuit stays open before letting one trial request
    # through (half-open) to test recovery. A failed trial re-opens the
    # circuit with a fresh cooldown; a successful one closes it. Only
    # meaningful when circuit_breaker_threshold is set. Default: 30.
    # circuit_breaker_cooldown_secs: 30
    # Optional rate limiter: maximum new attempts admitted per rolling
    # 1-second window. Distinct from max_concurrent — this bounds arrival
    # rate, not simultaneous in-flight requests (useful for a local
    # inference server that can only accept one request every N ms
    # regardless of concurrency). Rejected with reason
    # "provider_rate_limited", including for explicit `provider/model`
    # addressing. Implemented as a fixed (not sliding) 1-second window, so
    # up to ~2x the configured rate can be admitted across a window
    # boundary — acceptable for protecting a local server from being
    # flooded, not meant as a strict SLA-grade limiter. Unset means no
    # limit.
    # rate_limit_per_sec: 10

  openrouter:
    name: "openrouter"
    type: "open_router"
    base_url: "https://openrouter.ai/api"
    # Environment variable holding the secret API key (recommended over static keys)
    api_key_env: "OPENROUTER_API_KEY"
    timeout_secs: 120
    # Default HTTP headers to attach to every outgoing request
    default_headers:
      "HTTP-Referer": "http://localhost:8999"
      "X-Title": "Personal AI Gateway"

  vllm:
    name: "vllm"
    type: "vllm"
    base_url: "http://localhost:8000"
    timeout_secs: 300
    enabled: true

  ollama:
    name: "ollama"
    type: "ollama"
    base_url: "http://localhost:11434"
    timeout_secs: 300

  ollama_cloud:
    name: "ollama_cloud"
    type: "ollama_cloud"
    base_url: "https://api.ollama.com"
    api_key_env: "OLLAMA_API_KEY"
    timeout_secs: 300

  gemini:
    name: "gemini"
    type: "gemini"
    base_url: "https://generativelanguage.googleapis.com/v1beta/openai"
    api_key_env: "GEMINI_API_KEY"
    timeout_secs: 120

  huggingface:
    name: "huggingface"
    type: "hugging_face"
    base_url: "https://router.huggingface.co/v1"
    api_key_env: "HF_API_KEY"
    timeout_secs: 120


# ─── 3. Model Registry & Aliases ─────────────────────────────────────────────
# Defines logical model aliases that map to prioritized arrays of deployments.
models:
  coding-fast:
    name: "coding-fast"
    description: "Fast code completion preferring local inference with cloud fallback"
    candidates:
      # Candidate 1: Local llama.cpp
      - provider: "llama"
        model: "qwen3.5-9b"
        priority: 1
        max_context_length: 32768
        # Optional, used only by the "highest_quality"/"balanced" routing
        # policies: an operator-declared quality rank, lower is better (same
        # convention as `priority`). There's no benchmark/eval data in this
        # gateway to derive this automatically — it's explicit input, not
        # gateway-inferred. Left unset, this candidate sorts last under
        # those two policies.
        # quality_tier: 2
      # Candidate 2: Cloud OpenRouter fallback
      - provider: "openrouter"
        model: "qwen/qwen-2.5-coder-32b-instruct"
        priority: 2
    capabilities:
      tools: true
      streaming: true
      structured_output: true
      vision: false
      reasoning: false

  coding-strong:
    name: "coding-strong"
    description: "Frontier reasoning model for complex architectural tasks"
    candidates:
      - provider: "openrouter"
        model: "anthropic/claude-3.7-sonnet"
        priority: 1
      - provider: "gemini"
        model: "gemini-2.5-pro"
        priority: 2
    capabilities:
      tools: true
      vision: true
      reasoning: true
      streaming: true
      structured_output: true

  general:
    name: "general"
    description: "Everyday conversational assistant"
    candidates:
      - provider: "llama"
        model: "qwen3.5-9b"
        priority: 1
      - provider: "ollama"
        model: "llama3.2:latest"
        priority: 2
      - provider: "openrouter"
        model: "google/gemini-2.0-flash-001"
        priority: 3
    capabilities:
      streaming: true
      tools: true


# ─── 4. Routing Engine & Policy ──────────────────────────────────────────────
routing:
  # Routing algorithm. One of: "priority"/"ordered" (default), "round_robin",
  # "lowest_latency"/"fastest", "local_first", "cheapest", "highest_quality",
  # "balanced". See docs/ROUTING_AND_POLICIES.md's Stage 5 table for exactly
  # what each one ranks by. Can also be overridden per-request via
  # `routing.policy` in the request body.
  default_policy: "priority"

  # Maximum attempts per candidate provider before falling back
  max_retries_per_provider: 2

  # Per-attempt upstream network timeout in seconds
  attempt_timeout_secs: 120

  # Failure conditions that trigger fallback to the next candidate
  fallback_on:
    - "timeout"         # 504 / network connection timeout
    - "rate_limit"      # 429 Too Many Requests
    - "unavailable"     # 502 / 503 / connection refused
    - "server_error"    # 5xx HTTP response from upstream
    - "content_filter"  # 451 upstream safety / moderation block


# ─── 5. Observability & PostgreSQL Telemetry ─────────────────────────────────
telemetry:
  # Global toggle for telemetry recording
  enabled: true

  # Environment variable name containing the PostgreSQL connection string
  database_url_env: "DATABASE_URL"

  # Direct connection string (prefer database_url_env in production)
  # database_url: "postgres://postgres:password@localhost:5432/ai_gateway"

  # Maximum size of PostgreSQL connection pool
  pool_size: 10

  # Maximum events buffered before triggering an immediate batch write
  batch_size: 100

  # Periodic background flush interval in milliseconds
  flush_interval_ms: 1000

  # Store complete request bodies for dataset fine-tuning
  store_raw_bodies: true

  # Store individual SSE chunk receipt timestamps for inter-chunk jitter tracking
  store_stream_events: true

  # Path to an append-only JSONL file used as a last-resort durability
  # backstop when a telemetry batch fails to persist after all retries
  # (default: "telemetry_fallback.jsonl" in the working directory). Not a
  # full durable outbox with replay — just prevents an outage from silently
  # discarding data.
  fallback_log_path: "telemetry_fallback.jsonl"


# ─── 6. Deployment Pricing (optional) ────────────────────────────────────────
# Enables cost_records: keyed by "provider/model" (matching a ModelCandidate's
# provider/model fields). A deployment with no entry here simply produces no
# cost_records row for its generations — never a fabricated zero cost.
pricing:
  "llama/qwen3.5-9b":
    input_cost_per_1k: 0.0    # local inference — no per-token cost
    output_cost_per_1k: 0.0
  "openrouter/anthropic/claude-3.7-sonnet":
    input_cost_per_1k: 0.003
    output_cost_per_1k: 0.015
```

---

## 3. Environment Variable Interpolation

The gateway natively reads secrets from your environment:

| Variable | Description | Example |
|:---|:---|:---|
| `DATABASE_URL` | PostgreSQL connection URL | `postgres://user:pass@127.0.0.1:5432/gateway` |
| `OPENROUTER_API_KEY` | OpenRouter bearer token | `sk-or-v1-...` |
| `GEMINI_API_KEY` | Google AI Studio API key | `AIzaSy...` |
| `HF_API_KEY` | Hugging Face User Access Token | `hf_...` |
| `OLLAMA_API_KEY` | Ollama Cloud authentication key | `ollama_...` |
| `RUST_LOG` | Logging verbosity filter | `ai_gateway=debug,tower_http=info` |
