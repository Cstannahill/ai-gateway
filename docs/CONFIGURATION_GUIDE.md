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

**Reloading.** The file is re-read while the gateway runs - see section 5.

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
    # Optional health scoring beyond the ping-based binary healthy/unhealthy:
    # error rate (0.0-1.0) over the last 20 attempts (minimum 5 samples)
    # above which this provider is reported as "degraded" in /health,
    # /metrics, /providers, and the periodic ProviderSnapshot telemetry
    # event, even though the background health-check ping still says
    # healthy. Purely informational — never affects routing eligibility
    # (a degraded provider is still selected the same as a healthy one) and
    # never overrides a real ping-determined "unhealthy". Unset means
    # reported health stays purely the ping result.
    # degraded_error_rate_threshold: 0.5
    # Request-body fields to remove before sending to this provider. Unknown
    # top-level fields a client sends (logprobs, top_k, stream_options, ...)
    # are forwarded to every provider; list the ones a strict provider
    # rejects here instead of failing every request that carries them.
    # Also strips typed fields (e.g. parallel_tool_calls) and anything added
    # through provider_options. Unset means nothing is dropped.
    # drop_params: [logprobs, top_logprobs, logit_bias]

  openrouter:
    name: "openrouter"
    type: "open_router"
    base_url: "https://openrouter.ai/api"
    # Environment variable holding the secret API key (recommended over static keys)
    api_key_env: "OPENROUTER_API_KEY"
    # The variable may hold several comma-separated keys (OPENROUTER_API_KEY=k1,k2):
    # requests rotate across them, and a key answering 429/401/403 is skipped for
    # the Retry-After it gave (60s if none; 10 min for 401/403) while the same
    # request is retried on the next key. A single key behaves as before.
    timeout_secs: 120
    # Extra HTTP headers for every outgoing request. OpenRouter app-attribution
    # headers are added automatically (HTTP-Referer: https://ai-gateway.local,
    # X-OpenRouter-Title: AI Gateway); set them here only to override. Names are
    # case-insensitive. See docs/PROVIDER_INTEGRATION_GUIDE.md, "App attribution".
    # default_headers:
    #   "HTTP-Referer": "https://ai-gateway.local"
    #   "X-OpenRouter-Title": "AI Gateway"

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

  # Longest Retry-After (seconds) the gateway will sit through before
  # retrying the same provider. A provider answering 429/503 with a
  # Retry-After (or retry-after-ms) at or below this is waited out (at least
  # that long, with jittered exponential backoff otherwise); above it the
  # request falls through to the next candidate immediately and the provider
  # is skipped by routing ("provider_rate_limited") for the time it asked
  # for, capped at 5 minutes. 0 = never wait, always fall back. Default: 10.
  max_retry_after_secs: 10

  # Streaming responses are held until the provider produces its first output,
  # so a stream that errors, ends empty, or stalls BEFORE any output is retried
  # or failed over like any failed attempt instead of reaching the client broken.
  # The cost: response headers wait for the first token. Failures AFTER output
  # has started cannot be hidden and are never replayed. Default: true.
  stream_failover: true

  # With stream_failover: abandon a provider that has produced no output within
  # this many seconds and move on. Unset = wait indefinitely (a local model may
  # take minutes to process a long prompt); set it for cloud providers that
  # sometimes stall.
  # first_token_timeout_secs: 60

  # Failure conditions that trigger fallback to the next candidate
  fallback_on:
    - "timeout"         # 504 / network connection timeout
    - "rate_limit"      # 429 Too Many Requests
    - "unavailable"     # 502 / 503 / connection refused
    - "server_error"    # 5xx HTTP response from upstream
    - "content_filter"  # 451 upstream safety / moderation block


# ─── 4a. Response Caching ─────────────────────────────────────────────────────
# Exact-match caching for non-streaming chat completions only (a streaming
# request is never cached and never served from cache — see
# docs/ROUTING_AND_POLICIES.md's caching section for why). Disabled by
# default. This is an in-memory, per-process cache: it is never persisted
# and is lost on restart, and does not survive across a multi-instance
# deployment (not a concern for this gateway's personal, single-process
# scope).
caching:
  enabled: false
  # How long a cached response stays valid.
  ttl_secs: 300
  # Maximum distinct cached entries before the oldest-inserted one is
  # evicted to make room (oldest-inserted, not oldest-accessed — see
  # ResponseCache's doc comment in src/cache/mod.rs for why a full LRU
  # wasn't worth the complexity here).
  max_entries: 1000


# ─── 4b. llama-server management ──────────────────────────────────────────────
# Save models under aliases and start/stop/inspect a local llama-server via
# `/llama-server/*` (UI contract: docs/frontend/LLAMA_SERVER_UI.md). Disabled by
# default. It is the only feature that launches a local process, so it is
# guarded: validation refuses to start when this section is enabled but
# `server.host` is not loopback (set `allow_non_loopback: true` to override
# knowingly), and every /llama-server/* request that carries a browser
# `Origin` (or a non-loopback `Host`) is rejected with 403 — the frontend must
# reach it through its server-side proxy.
llama_server:
  enabled: false
  # Path to the real llama-server executable. A .cmd/.bat shim is rejected at
  # startup: stopping a shim would leave the actual server process orphaned.
  # (S:\bin\llama-server.cmd is such a shim; use the .exe it points to.)
  binary: 'S:\llama.cpp\build\bin\Release\llama-server.exe'
  # Passed as --host / --port; also where status probes connect. Keep `port`
  # equal to the port in the `llama` provider's base_url so the gateway routes
  # to whatever the manager starts.
  host: 127.0.0.1
  port: 8080
  # Which entry under `providers:` talks to this server (default "llama"; null
  # unlinks). Linked, that provider (a) lists the saved models by alias in
  # /v1/models even while nothing is loaded, (b) reports "degraded" instead of
  # "unhealthy" while no model is loaded but one can be, so routing still sends
  # it requests, and (c) with auto_swap, loads the saved model a request names.
  # Its base_url must point at host:port above (loopback) or startup fails.
  # If no provider has this name, the link is skipped with a warning.
  provider: llama
  # A request to `provider` naming a saved model (its alias, case-insensitive,
  # or its .gguf path) that is not the loaded one stops the current model and
  # starts the requested one before dispatching. Any other model name is passed
  # through to whatever is loaded. A .gguf path that is not saved is refused.
  auto_swap: true
  # A swap waits this long for in-flight requests on the current model to
  # finish (it never kills a model mid-request); on timeout the request fails
  # over to the next candidate.
  swap_drain_timeout_secs: 120
  # Saved models + default args (JSON, atomic writes). A corrupt file fails
  # startup with a clear message and is never overwritten.
  state_file: config/llama_models.json
  # Optional directories the UI's "choose from scanned files" picker may list
  # *.gguf files from (depth <= 4). Nothing outside these is ever listed.
  scan_dirs: []
  # Browser origins allowed to call /llama-server/* directly. Leave empty:
  # only origin-less callers (the frontend's server-side proxy, curl) pass.
  allowed_origins: []
  allow_non_loopback: false
  # How long to wait for /health to report ready before marking the start
  # failed and killing the process. Big models load slowly.
  start_timeout_secs: 300
  # Captured stdout/stderr lines kept in memory for the log view.
  log_lines: 2000
# Model paths themselves are validated when saved (absolute, exists, .gguf) and
# re-checked at start. Arguments: `-m/--model`, `--mmproj`, `--host`, `--port`
# and the download/help/exit-immediately flags are owned by the manager and
# rejected; everything else (e.g. `-ngl all -fa on -np 1 --ui-mcp-proxy`) is
# passed through. A hard kill of the gateway (no graceful shutdown) leaves the
# server running; it then shows up as an "external" server.


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

The gateway natively reads secrets from your environment. It also loads a
`.env` file from the current directory at startup (or the file given with
`--env-file <PATH>`); variables already set in the real environment win over the
file. Copy `.env.example` to `.env` to start. See `COMMANDS.md` section 2.

| Variable | Description | Example |
|:---|:---|:---|
| `DATABASE_URL` | PostgreSQL connection URL | `postgres://user:pass@127.0.0.1:5432/gateway` |
| `OPENROUTER_API_KEY` | OpenRouter bearer token | `sk-or-v1-...` |
| `GEMINI_API_KEY` | Google AI Studio API key | `AIzaSy...` |
| `HF_API_KEY` | Hugging Face User Access Token | `hf_...` |
| `OLLAMA_API_KEY` | Ollama Cloud authentication key | `ollama_...` |
| `RUST_LOG` | Logging verbosity filter | `ai_gateway=debug,tower_http=info` |

---

## 5. Reloading configuration without a restart

The gateway re-reads `gateway.yaml` and the env file (`.env`, or `--env-file`) in two ways:

- **Automatically** when either file changes on disk. Files are checked every 2 seconds and a change is applied only once it has stopped changing for a full check, so an editor's half-finished save is not loaded. A version that fails to load is reported once (in the log and at `GET /admin/reload`) and is not retried until the file changes again.
- **On demand** with `curl -X POST http://127.0.0.1:8999/admin/reload`. The response (`200`, or `422` if the new configuration was rejected) lists what changed: providers and model aliases added/updated/removed, environment variables whose value changed (names only, never values), and sections that need a restart. `GET /admin/reload` shows the last result.

**What is reloaded:** `providers` (added, removed, changed), `models`, `routing`.

**What needs a restart** (reported in `restart_required`, and the running value stays in effect): `server` (the listening socket), `telemetry` and `pricing` (the writer pipeline keeps its own copy), `caching`, and `llama_server` - it decides which binary the gateway will execute, so it is deliberately never changed behind your back.

**Safety.** The whole new file is parsed and validated, and every new provider adapter built, before anything is swapped; if any step fails, nothing changes. Requests already running finish on the configuration they started with, including streams whose provider was just removed.

**What carries over.** A provider whose settings and API keys did not change keeps its running adapter, so its health, request counters, circuit-breaker state, rate-limit cooldown and API-key rotation state survive a reload. A provider whose settings changed is rebuilt (counters kept, health re-checked within 30 s).

**Environment file.** On reload the env file is re-read, so adding or rotating an API key in `.env` takes effect when you save - no restart. Precedence is the same as at startup: a variable set in the real environment (shell profile, system settings) always wins over the file, and the log warns (names only) when it hides a different value from the file. Providers whose key variable changed are rebuilt with the new keys. The process environment itself is not modified after startup.

**Access.** `/admin/*` is guarded like `/llama-server/*`: requests that carry a browser `Origin`, a cross-site `Sec-Fetch-Site`, or a non-loopback `Host` are refused with `403`. `curl` and the dashboard's server-side proxy send none of those. A dashboard that wants a "Reload" button must add `POST /admin/reload` to its proxy allow-list (the existing `/api/gateway/*` proxy is GET-only).
