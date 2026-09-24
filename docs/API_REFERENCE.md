# HTTP API Reference

The **AI Gateway** exposes an OpenAI-compatible HTTP API alongside native monitoring and management endpoints.

---

## Endpoint Summary

| Endpoint | Method | Purpose | Protocol |
|:---|:---:|:---|:---|
| [`/v1/chat/completions`](#1-post-v1chatcompletions) | `POST` | Chat completion inference | JSON or Server-Sent Events (SSE) |
| [`/v1/responses`](#2-post-v1responses) | `POST` | Responses API inference | JSON |
| [`/v1/embeddings`](#3-post-v1embeddings) | `POST` | Text embedding vector generation | JSON |
| [`/v1/models`](#4-get-v1models) | `GET` | Catalog of configured & discovered models | JSON |
| [`/health`](#5-get-health) | `GET` | Gateway health check | JSON |
| [`/metrics`](#6-get-metrics) | `GET` | Real-time performance & latency metrics | JSON |
| [`/providers`](#7-get-providers) | `GET` | Runtime health & load per provider | JSON |
| [`/v1/analytics/summary`](#8-get-v1analyticssummary) | `GET` | Date-bucketed activity & usage summary | JSON |
| [`/v1/analytics/requests`](#9-get-v1analyticsrequests) | `GET` | List/filter recorded requests | JSON |
| [`/v1/analytics/requests/{request_id}`](#10-get-v1analyticsrequestsrequest_id) | `GET` | Full detail for one request | JSON |
| [`/v1/analytics/requests/{request_id}/timeline`](#11-get-v1analyticsrequestsrequest_idtimeline) | `GET` | Chunk-level stream timeline | JSON |
| [`/v1/analytics/providers/comparison`](#12-get-v1analyticsproviderscomparison) | `GET` | Cross-provider TTFT/TPS/success-rate comparison | JSON |
| [`/v1/analytics/fallbacks`](#13-get-v1analyticsfallbacks) | `GET` | Fallback frequency & root cause | JSON |
| [`/v1/analytics/costs`](#14-get-v1analyticscosts) | `GET` | Estimated cost summary & pricing coverage | JSON |
| [`/v1/analytics/persistence`](#15-get-v1analyticspersistence) | `GET` | Telemetry write-pipeline health | JSON |

---

## 1. POST `/v1/chat/completions`

Generates chat completions from a logical model alias or explicit provider deployment. Supports both non-streaming responses and real-time Server-Sent Events (SSE).

### Request Headers
- `Content-Type: application/json`
- `Authorization: Bearer <token>` *(Optional if gateway authentication is disabled)*
- `x-request-id: <uuid>` *(Optional: client-provided request ID for tracing)*

### Request Parameters

```json
{
  "model": "coding-fast",
  "messages": [
    {
      "role": "system",
      "content": "You are a concise programming assistant."
    },
    {
      "role": "user",
      "content": "Write a Rust function to check for palindromes."
    }
  ],
  "temperature": 0.7,
  "top_p": 0.95,
  "max_tokens": 1024,
  "stream": false,
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "lookup_docs",
        "description": "Fetch documentation for a Rust crate",
        "parameters": {
          "type": "object",
          "properties": {
            "crate_name": { "type": "string" }
          },
          "required": ["crate_name"]
        }
      }
    }
  ],
  "provider_options": {
    "llama_cpp": {
      "min_p": 0.05,
      "repeat_penalty": 1.1
    }
  }
}
```

### Response (Non-Streaming: `stream: false`)
```json
{
  "id": "chatcmpl-01921a4f-5b68-7182-938b-829d10e01234",
  "object": "chat.completion",
  "created": 1758498200,
  "model": "qwen3.5-9b",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "pub fn is_palindrome(s: &str) -> bool {\n    let chars: Vec<char> = s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();\n    chars.iter().eq(chars.iter().rev())\n}",
        "reasoning_content": "Analyzing palindrome conditions. Case insensitivity and alphanumeric filtering are best practice."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 32,
    "completion_tokens": 64,
    "total_tokens": 96,
    "prompt_tokens_details": { "cached_tokens": 0 },
    "completion_tokens_details": { "reasoning_tokens": 18 }
  }
}
```

### Response (Streaming: `stream: true`)
Returned as `Content-Type: text/event-stream`. Each chunk follows standard OpenAI format:

```text
data: {"id":"chatcmpl-01921a4f","object":"chat.completion.chunk","created":1758498200,"model":"qwen3.5-9b","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"chatcmpl-01921a4f","object":"chat.completion.chunk","created":1758498200,"model":"qwen3.5-9b","choices":[{"index":0,"delta":{"content":"pub fn"},"finish_reason":null}]}

data: {"id":"chatcmpl-01921a4f","object":"chat.completion.chunk","created":1758498200,"model":"qwen3.5-9b","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

```

#### Example cURL
```bash
curl -X POST http://localhost:8999/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding-fast",
    "messages": [{"role": "user", "content": "Explain async/await in Rust"}],
    "stream": true
  }'
```

---

## 2. POST `/v1/responses`

Executes generation against the Responses API interface, returning canonical response objects.

### Request Body
```json
{
  "model": "coding-fast",
  "input": "Explain memory safety in Rust in 3 sentences."
}
```

### Response
```json
{
  "id": "resp_01921a4f-5b68-7182-938b-829d10e01234",
  "object": "response",
  "created_at": 1758498200,
  "model": "qwen3.5-9b",
  "status": "completed",
  "output": [
    {
      "type": "message",
      "role": "assistant",
      "content": "Rust guarantees memory safety through its ownership and borrowing system enforced at compile time. It eliminates data races, dangling pointers, and double-free errors without requiring a garbage collector. When necessary, unsafe blocks allow developers to opt out of these checks explicitly."
    }
  ],
  "usage": {
    "prompt_tokens": 18,
    "completion_tokens": 52,
    "total_tokens": 70
  }
}
```

### Response (Streaming: `stream: true`)
Returned as `Content-Type: text/event-stream`. This is a documented subset of OpenAI's Responses streaming protocol — text deltas, a terminal completion/failure event, and a passthrough error event — not full parity (no tool-call or reasoning-summary streaming events, no multi-item output):

```text
event: response.created
data: {"type":"response.created","response":{"id":"resp_...","object":"response","created_at":1758498200,"model":"qwen3.5-9b","status":"in_progress","output":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_...","output_index":0,"content_index":0,"delta":"Rust guarantees"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_...","object":"response","created_at":1758498200,"model":"qwen3.5-9b","status":"completed","output":[{"type":"message","id":"msg_...","role":"assistant","content":[{"type":"output_text","text":"Rust guarantees memory safety..."}]}]}}
```

If the upstream provider errors mid-stream, the client instead sees an `error` event followed by a terminal `response.failed` event rather than `response.completed`.

---

## 3. POST `/v1/embeddings`

Generates embedding vector representations for given input text.

### Request Body
```json
{
  "model": "llama/nomic-embed-text",
  "input": "Search query about Rust memory management"
}
```

### Response
```json
{
  "object": "list",
  "data": [
    {
      "object": "embedding",
      "index": 0,
      "embedding": [0.0123, -0.0456, 0.0891, "..."]
    }
  ],
  "model": "nomic-embed-text",
  "usage": {
    "prompt_tokens": 8,
    "total_tokens": 8
  }
}
```

---

## 4. GET `/v1/models`

Lists all available models. Aggregates statically configured logical aliases and dynamically discovered models from active provider adapters.

### Response
```json
{
  "object": "list",
  "data": [
    {
      "id": "coding-fast",
      "object": "model",
      "created": 0,
      "owned_by": "ai-gateway",
      "capabilities": {
        "tools": true,
        "vision": false,
        "reasoning": false,
        "structured_output": true,
        "streaming": true,
        "embeddings": false,
        "context_window": null,
        "extra": []
      },
      "providers": ["llama", "openrouter"]
    },
    {
      "id": "coding-strong",
      "object": "model",
      "created": 0,
      "owned_by": "ai-gateway",
      "capabilities": {
        "tools": true,
        "vision": true,
        "reasoning": true,
        "structured_output": true,
        "streaming": true,
        "embeddings": false,
        "context_window": null,
        "extra": []
      },
      "providers": ["openrouter", "gemini"]
    }
  ]
}
```

---

## 5. GET `/health`

Health check endpoint for container orchestrators (Docker, Kubernetes) and load balancers.

### Response
```json
{
  "status": "healthy",
  "gateway": "ai-gateway",
  "version": "0.1.0",
  "providers": 3
}
```

---

## 6. GET `/metrics`

Live gateway performance statistics, uptime, error rates, and average latencies.

### Response
```json
{
  "gateway": {
    "version": "0.1.0",
    "uptime_seconds": 86400,
    "total_requests": 14205,
    "total_errors": 12,
    "active_requests": 2
  },
  "providers": [
    {
      "name": "llama",
      "provider_type": "llama_cpp",
      "enabled": true,
      "health": "healthy",
      "total_requests": 11200,
      "total_errors": 4,
      "active_requests": 1,
      "avg_latency_ms": 142.5,
      "error_rate": 0.00035
    },
    {
      "name": "openrouter",
      "provider_type": "open_router",
      "enabled": true,
      "health": "healthy",
      "total_requests": 3005,
      "total_errors": 8,
      "active_requests": 1,
      "avg_latency_ms": 680.2,
      "error_rate": 0.00266
    }
  ]
}
```

---

## 7. GET `/providers`

Returns the operational status, health, and current load for each registered provider instance.

### Response
```json
{
  "providers": [
    {
      "name": "llama",
      "provider_type": "llama_cpp",
      "health": "healthy",
      "enabled": true,
      "active_requests": 0,
      "total_requests": 11200,
      "total_errors": 4,
      "avg_latency_ms": 142.5
    },
    {
      "name": "openrouter",
      "provider_type": "open_router",
      "health": "healthy",
      "enabled": true,
      "active_requests": 1,
      "total_requests": 3005,
      "total_errors": 8,
      "avg_latency_ms": 680.2
    }
  ]
}
```

---

## 8. GET `/v1/analytics/summary`

Read-only query API over the telemetry schema (docs/REMEDIATION_PLAN.md Phase 4 item 1). Every `/v1/analytics/*` endpoint requires a reachable `DATABASE_URL` — if the gateway started without one (or couldn't connect at startup), every endpoint in this section returns `503 analytics_unavailable` instead of an empty/misleading result.

`docs/frontend/API_GAPS.md` is a frontend-driven gap analysis of this API (written against the dashboard's actual needs) and tracks known remaining limitations — some closed since it was written (see docs/REMEDIATION_PLAN.md's latest status block for which), some still open. Read it before assuming a field this section doesn't mention is simply missing by oversight; several are deliberate, documented scope boundaries (e.g. no keyset pagination yet, no usage-observation finality model yet).

A date-bucketed activity/usage summary over a half-open `[from, to)` window, at `minute`/`hour`/`day` granularity. `from`/`to` accept RFC3339 timestamps — use the `Z` UTC suffix rather than `+00:00` unless you percent-encode the `+` (a literal `+` in a query string decodes as a space). Both default: `to` defaults to now, `from` defaults to a bucket-size-appropriate lookback (60 minutes / 24 hours / 30 days) if omitted. The range is capped at 500 buckets — narrow the range or choose a coarser bucket if you exceed it (`400 range_too_large`).

### Query Parameters
| Parameter | Type | Description |
|:---|:---|:---|
| `from` | RFC3339 timestamp | Inclusive start of the window. Default: bucket-appropriate lookback from `to`. |
| `to` | RFC3339 timestamp | Exclusive end of the window. Default: now. |
| `bucket` | string | One of `minute`, `hour`, `day`. Default: `hour`. |
| `model` | string | Filter to a `requested_model` or `resolved_model` exact match. |
| `provider` | string | Filter to a `serving_provider` exact match — the *final* provider, never an attempted-but-failed one. |

### Response
```json
{
  "scope": {
    "from": "2026-09-22T00:00:00Z",
    "to": "2026-09-23T00:00:00Z",
    "bucket": "hour",
    "provider_semantics": "serving_provider (the final provider that actually served the request; an attempted-but-failed provider never matches this filter)"
  },
  "requests": { "total": 412, "successful": 398, "failed": 9, "pending": 5 },
  "attempts": { "total": 431, "failed": 33 },
  "usage": { "prompt": 812400, "completion": 265900, "cached": 41200, "reasoning": 18300, "known_requests": 401, "eligible_requests": 412 },
  "series": [
    {
      "bucket_start": "2026-09-22T00:00:00Z",
      "requests_total": 18, "requests_successful": 17, "requests_failed": 1, "requests_pending": 0,
      "attempts_total": 19, "attempts_failed": 1,
      "usage_prompt": 33200, "usage_completion": 11100, "usage_cached": 1800, "usage_reasoning": 900, "usage_known_requests": 17
    }
  ]
}
```

`requests.pending` means the request was received but no `RequestCompleted` telemetry event has landed yet (`completed_at IS NULL`) — distinct from `requests.failed` (`success = false`, a genuine terminal failure). `usage` fields are `null`, not `0`, for a scope with no recorded usage at all — absence is a real "unknown," not a measured zero. Usage is de-duplicated to one canonical (most-recently-observed) row per attempt before summing, so an attempt with multiple cumulative `UsageRecorded` updates is never double-counted — same as `/v1/analytics/providers/comparison`.

---

## 9. GET `/v1/analytics/requests`

Lists recorded requests using **keyset pagination**, ordered by `(received_at DESC, request_id DESC)`. Not offset pagination: a prior `offset`-based version could duplicate or omit rows when a new request was inserted between two page fetches, because an offset counts row positions that shift under concurrent inserts. A keyset cursor names a stable position in the data instead — see `docs/frontend/API_GAPS.md` item 6.

### Query Parameters
| Parameter | Type | Description |
|:---|:---|:---|
| `limit` | integer | Max rows to return. Default `50`, capped at `500`. |
| `cursor` | string | Opaque cursor from a previous response's `next_cursor`. Omit for the first page. |
| `from` | RFC3339 timestamp | Inclusive lower bound on `received_at`. |
| `to` | RFC3339 timestamp | Exclusive upper bound on `received_at`. |
| `model` | string | Filter to a `requested_model` or `resolved_model` exact match. |
| `provider` | string | Filter to a `serving_provider` (final) exact match. |
| `attempted_provider` | string | Filter to requests where at least one attempt targeted this provider, regardless of outcome — distinct from `provider`, which only matches the provider that actually served the request. |
| `application` | string | Filter to an exact `application` match. |
| `task` | string | Filter to an exact `task` match. |
| `tag` | string | Filter to requests carrying this exact tag. |
| `success` | boolean | Filter to successful (`true`) or failed (`false`) requests only. |

### Response
```json
{
  "requests": [
    {
      "request_id": "01921a4f-5b68-7182-938b-829d10e01234",
      "client_correlation_id": null,
      "requested_model": "coding-fast",
      "resolved_model": "qwen2.5-coder:7b",
      "serving_provider": "ollama",
      "success": true,
      "streaming": true,
      "total_latency_us": 842103,
      "received_at": "2026-09-23T03:26:29.507051300Z"
    }
  ],
  "next_cursor": "1758598000000000_01921a3e-...",
  "as_of": "2026-09-23T03:27:00.000000000Z",
  "limit": 50
}
```

`next_cursor` is `null` when there is no further page. `as_of` is the server time the query ran — pair it with a `from`-bounded follow-up query to check for new rows without re-deriving that from the page's own contents.

---

## 10. GET `/v1/analytics/requests/{request_id}`

Full detail for one request: the parent row, every provider attempt (in fallback-chain order, with start/finish spans), any generated content, token usage, the routing decision(s) behind candidate selection, structured errors, and per-attempt cost records — everything needed to answer "what actually happened for this request, and why" without hand-writing a join. Returns `404` if the ID doesn't exist.

Each generation carries a stable `id` and its own `choice_index`: an `n > 1` attempt produces one `generations` row per choice, not one row merging every choice's content together. `status` distinguishes a generation reached via a genuine terminal event (`"complete"`) from one reached via a mid-stream error with partial content preserved (`"partial"`) — see docs/frontend/API_GAPS.md item 5.

### Response
```json
{
  "request": { "request_id": "...", "requested_model": "coding-fast", "resolved_model": "qwen2.5-coder:7b", "success": true, "canonical_messages": [ /* ... */ ], "raw_request": { /* ... */ } },
  "attempts": [
    {
      "attempt_number": 1, "provider": "ollama", "model": "qwen2.5-coder:7b", "status": "success",
      "duration_us": 842103, "started_at": "2026-09-23T03:26:29.500Z", "finished_at": "2026-09-23T03:26:30.342Z",
      "duration_definition": "duration_us/finished_at span the time from provider dispatch (Started) to this attempt's terminal event (Completed/Error). For a streaming attempt this is measured after InstrumentedStream fully drains the response, not at TCP connection or response-header time — see docs/ROUTING_AND_POLICIES.md's Stage 3 notes and docs/telemetry writer.rs's Connected event doc comment.",
      "first_byte_at": "...", "first_token_at": "...", "synthetic_timing": false
    }
  ],
  "generations": [
    { "id": 4821, "attempt_number": 1, "choice_index": 0, "content": "...", "reasoning_content": null, "reasoning_visibility": "none", "tool_calls": null, "status": "complete" }
  ],
  "usage": [
    {
      "id": 991, "attempt_number": 1, "observed_at": "2026-09-23T03:26:30.100Z",
      "prompt_tokens": 128, "completion_tokens": 256, "total_tokens": 384, "cached_tokens": null, "reasoning_tokens": null,
      "source": "stream_final", "kind": "cumulative"
    }
  ],
  "usage_canonical": [
    { "attempt_number": 1, "prompt_tokens": 128, "completion_tokens": 256, "total_tokens": 384, "cached_tokens": null, "reasoning_tokens": null, "observation_count": 1, "quality": "single_observation" }
  ],
  "routing": [
    {
      "timestamp": "2026-09-23T03:26:29.480Z", "requested_model": "coding-fast", "resolution_type": "alias",
      "candidates_considered": 2, "candidates_rejected": 1,
      "rejection_reasons": [{ "provider": "llama", "model": "qwen2.5-coder:7b", "reason": "provider_unavailable" }],
      "selected_provider": "ollama", "selected_model": "qwen2.5-coder:7b", "policy_applied": "ordered", "short_circuit": null
    }
  ],
  "errors": [
    { "attempt_number": 1, "timestamp": "...", "error_code": "server_error", "error_message": "upstream 503", "status_code": 503, "provider": "llama", "retryable": true }
  ],
  "costs": [
    { "timestamp": "...", "estimated_cost": 2.0, "actual_cost": null, "currency": "USD", "provider": "ollama", "model": "qwen2.5-coder:7b", "input_cost": 1.0, "output_cost": 1.0 }
  ]
}
```

`duration_definition` is a fixed string, not a per-row measurement — it states the actual semantics of `duration_us`/`finished_at` explicitly so a caller doesn't infer full dispatch-to-completion or TCP-level latency from it (`docs/frontend/API_GAPS.md` item 5: "do not infer TCP connection time from response-header time"). `costs` reflects only estimated charges from configured `pricing` (see §14) — `actual_cost` is schema-only and always `null` today; a request with no configured price for its deployment has an empty `costs` array, not a fabricated zero-cost entry.

`usage` preserves every raw usage observation exactly as received — `kind` is always `"cumulative"` today (every provider this gateway supports reports one cumulative token total per attempt, never incremental deltas; see `docs/frontend/API_GAPS.md` item 2), and `source` says which code path produced it (`"stream_final"` or `"non_streaming"`). `usage_canonical` is a separate, independently derived array with one entry per attempt that has any usage at all: `quality` is `"single_observation"` in the normal case (exactly one observation), `"duplicate_observations"` if more than one identical observation arrived (e.g. a redelivered event — harmless), or `"conflicting_observations"` if more than one observation arrived with genuinely different values (a real anomaly worth investigating, not silently averaged or summed — the canonical values shown are simply the most recently observed ones). An attempt with no usage observation at all has no entry in `usage_canonical` — absence means unknown, not zero.

---

## 11. GET `/v1/analytics/requests/{request_id}/timeline`

The chunk-level stream timeline for a request, using cursor pagination ordered by `(attempt_number, sequence)` — stream events are append-only per attempt, so (unlike the request list) there's no concurrent-insert race to guard against; the cursor exists purely to bound response size for a long stream. Each event carries `synthetic`: `true` means the event was fabricated by replaying a single non-streaming HTTP response as a one-shot event sequence (all landing within microseconds of each other), not observed from a genuine live stream. This endpoint deliberately does not filter `synthetic` events out — callers decide (see docs/TELEMETRY_AND_DATABASE.md §4).

### Query Parameters
| Parameter | Type | Description |
|:---|:---|:---|
| `after` | string | Opaque cursor from a previous response's `next_cursor`. Omit for the first page. |
| `limit` | integer | Max events to return. Default `1000`, capped at `5000`. |

### Response
```json
{
  "request_id": "01921a4f-5b68-7182-938b-829d10e01234",
  "events": [
    { "attempt_number": 1, "timestamp": "2026-09-23T03:26:29.507Z", "sequence": 1, "chunk_type": "content", "byte_count": 5, "synthetic": false }
  ],
  "next_cursor": null,
  "complete": true
}
```

`complete: true` means every event for this request, across however many `after`-chained calls it took, has now been returned exactly once — `next_cursor` is `null` in that case. While `next_cursor` is non-null, `complete` is `false`.

---

## 12. GET `/v1/analytics/providers/comparison`

Per-provider/model aggregate stats: attempt volume/success, average latency, average tokens/sec, and TTFT (mean + p50/p95) — each average carries its own sample count. Every timing average excludes attempts where `synthetic_timing = true` (non-streaming reconstructions) so real streaming performance isn't diluted by fabricated near-zero latencies. A canonical (most-recently-observed) usage row is selected per attempt before joining, so an attempt with more than one `UsageRecorded` update is never counted more than once. TTFT is measured from `provider_attempts.timestamp` and deliberately includes failed attempts that still produced a first-token marker — it's a dispatch/reachability signal, not a success-only metric.

### Response
```json
{
  "providers": [
    {
      "provider": "ollama",
      "model": "qwen2.5-coder:7b",
      "total_attempts": 120,
      "successful_attempts": 116,
      "avg_duration_ms": 842.1,
      "duration_sample_count": 116,
      "avg_tokens_per_sec": 38.7,
      "throughput_sample_count": 110,
      "avg_ttft_ms": 210.4,
      "p50_ttft_ms": 195.0,
      "p95_ttft_ms": 340.2,
      "ttft_sample_count": 118,
      "synthetic_attempts": 4
    }
  ]
}
```

---

## 13. GET `/v1/analytics/fallbacks`

Fallback frequency and root cause: for each of the most recent successful requests that had at least one non-success attempt along the way, the *full ordered attempt chain* (not just the failures) so the whole recovery sequence — same-provider retry, model switch, or provider switch — is reconstructable. The 200-row cap applies to distinct requests, not joined attempt rows; `truncated: true` means older recovered requests exist beyond this sample.

### Response
```json
{
  "fallbacks": [
    {
      "request_id": "01921a4f-5b68-7182-938b-829d10e01234",
      "succeeded_provider": "openrouter",
      "attempts": [
        {
          "attempt_number": 1,
          "timestamp": "2026-09-23T03:26:29.507Z",
          "provider": "llama",
          "model": "qwen2.5-coder:7b",
          "status": "failed",
          "error_code": "server_error",
          "error_message": "upstream 503"
        },
        {
          "attempt_number": 2,
          "timestamp": "2026-09-23T03:26:29.612Z",
          "provider": "openrouter",
          "model": "qwen2.5-coder:7b",
          "status": "success",
          "error_code": null,
          "error_message": null
        }
      ]
    }
  ],
  "sample_scope": "up to 200 most recent successful requests with at least one non-success attempt, ordered by received_at DESC",
  "truncated": false
}
```

---

## 14. GET `/v1/analytics/costs`

Cost summary: total estimated charge, pricing coverage, and a per-provider/model breakdown, computed from `cost_records` (itself populated only when `config.pricing` has an entry for a given `"provider/model"` deployment — see `docs/CONFIGURATION_GUIDE.md`). A missing price yields an *unpriced* attempt, not a zero-cost one; an explicitly configured zero price is priced. `billed_cost` is always `null` — this is an estimate from configured prices, never an actual provider invoice.

### Query Parameters
| Parameter | Type | Description |
|:---|:---|:---|
| `provider` | string | Filter to one provider (exact match). |
| `model` | string | Filter to one model (exact match). |

### Response
```json
{
  "currency": "USD",
  "estimated_token_charge": 2.00,
  "billed_cost": null,
  "priced_attempts": 1,
  "unpriced_attempts": 1,
  "eligible_attempts": 2,
  "pricing_source": "config/gateway.yaml `pricing` table (static, unversioned per-deployment $/1K-token prices)",
  "breakdowns": [
    {
      "provider": "openrouter",
      "model": "anthropic/claude-3.5-sonnet",
      "priced_attempts": 1,
      "estimated_cost": 2.00,
      "input_cost": 1.00,
      "output_cost": 1.00
    }
  ]
}
```

---

## 15. GET `/v1/analytics/persistence`

Is the telemetry writer actually flushing, how far behind is it, and how many events have been dropped or are sitting in the local fallback log — distinct from `GET /health`, which reports provider status only and says nothing about whether data capture is actually working (docs/frontend/API_GAPS.md item 8, persistence-health half; the auth half of that item is explicitly out of scope for this personal, loopback-only deployment).

Unlike every other `/v1/analytics/*` endpoint, this one does **not** require `analytics_pool` and never returns `503` for a missing one — it reports on the write-side pipeline, which is independent of the read-side query pool.

### Response
```json
{
  "mode": "database",
  "healthy": true,
  "channel": { "capacity": 1000, "pending": 0 },
  "last_flush_at": "2026-09-23T04:12:00.000Z",
  "last_flush_ok": true,
  "consecutive_flush_failures": 0,
  "fallback_log_pending": false,
  "dropped_events_total": 0
}
```

- `mode` is `"database"` (connected and writing), `"unavailable"` (telemetry enabled but no database was reachable at startup — events are logged, not persisted, and this writer does not retry the initial connection), or `"disabled"` (`telemetry.enabled: false` in config — a deliberate choice, not a failure).
- `healthy` is a narrow, honest signal: `true` only when `mode == "database"` *and* the last flush succeeded *and* nothing is sitting in the fallback log. `"unavailable"` and `"disabled"` are both data-capture problems worth surfacing plainly, so neither is ever `healthy`.
- `channel.pending` is read directly from the live `mpsc` channel between route handlers and the background writer (not tracked separately), so it can never drift from reality — a nonzero, growing value means the writer isn't keeping up.
- `dropped_events_total` counts events that never reached the writer at all because the channel was full — a distinct, more severe signal than a delayed flush.
- `fallback_log_pending` is `true` when a flush exhausted its retries and fell back to the local JSONL log (see `docs/ARCHITECTURE.md`'s Step 4) — the pipeline degrades to "delayed" rather than "lost," but this flag says that degradation is currently active.

---

## Error Responses

When an error occurs, the gateway returns standard OpenAI-compatible error payloads:

```json
{
  "error": {
    "message": "Model 'unknown-model' not found in registry or active providers.",
    "type": "invalid_request_error",
    "code": "model_not_found"
  }
}
```

### Common Error Codes

| HTTP Status | Error Type / Code | Cause |
|:---:|:---|:---|
| `400` | `invalid_request_error` | Missing required parameters, invalid JSON schema, or bad request |
| `404` | `model_not_found` | Requested model alias or provider deployment is not registered |
| `422` | `unprocessable_entity` | Parameter validation failed (e.g. invalid temperature range) |
| `502` | `all_providers_failed` | All candidate providers in the fallback plan failed or timed out |
| `504` | `gateway_timeout` | The candidate attempt exceeded `attempt_timeout_secs` |
