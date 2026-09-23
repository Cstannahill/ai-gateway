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
| [`/v1/analytics/requests`](#8-get-v1analyticsrequests) | `GET` | List/filter recorded requests | JSON |
| [`/v1/analytics/requests/{request_id}`](#9-get-v1analyticsrequestsrequest_id) | `GET` | Full detail for one request | JSON |
| [`/v1/analytics/requests/{request_id}/timeline`](#10-get-v1analyticsrequestsrequest_idtimeline) | `GET` | Chunk-level stream timeline | JSON |
| [`/v1/analytics/providers/comparison`](#11-get-v1analyticsproviderscomparison) | `GET` | Cross-provider TTFT/TPS/success-rate comparison | JSON |
| [`/v1/analytics/fallbacks`](#12-get-v1analyticsfallbacks) | `GET` | Fallback frequency & root cause | JSON |

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

## 8. GET `/v1/analytics/requests`

Read-only query API over the telemetry schema (docs/REMEDIATION_PLAN.md Phase 4 item 1). Every `/v1/analytics/*` endpoint requires a reachable `DATABASE_URL` — if the gateway started without one (or couldn't connect at startup), every endpoint in this section returns `503 analytics_unavailable` instead of an empty/misleading result.

Lists recorded requests, newest first.

### Query Parameters
| Parameter | Type | Description |
|:---|:---|:---|
| `limit` | integer | Max rows to return. Default `50`, capped at `500`. |
| `offset` | integer | Pagination offset. Default `0`. |
| `model` | string | Filter to a `requested_model` or `resolved_model` exact match. |
| `provider` | string | Filter to a `serving_provider` exact match. |
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
  "limit": 50,
  "offset": 0
}
```

---

## 9. GET `/v1/analytics/requests/{request_id}`

Full detail for one request: the parent row, every provider attempt (in fallback-chain order), any generated content, and token usage — everything needed to answer "what actually happened for this request" without hand-writing a join. Returns `404` if the ID doesn't exist.

### Response
```json
{
  "request": { "request_id": "...", "requested_model": "coding-fast", "resolved_model": "qwen2.5-coder:7b", "success": true, "canonical_messages": [ /* ... */ ], "raw_request": { /* ... */ } },
  "attempts": [
    { "attempt_number": 1, "provider": "ollama", "model": "qwen2.5-coder:7b", "status": "success", "duration_us": 842103, "first_byte_at": "...", "first_token_at": "...", "synthetic_timing": false }
  ],
  "generations": [
    { "attempt_number": 1, "content": "...", "reasoning_content": null, "reasoning_visibility": "none", "tool_calls": null }
  ],
  "usage": [
    { "attempt_number": 1, "prompt_tokens": 128, "completion_tokens": 256, "total_tokens": 384, "cached_tokens": null, "reasoning_tokens": null }
  ]
}
```

---

## 10. GET `/v1/analytics/requests/{request_id}/timeline`

The chunk-level stream timeline for a request, ordered by attempt and sequence — the raw material for TTFT/inter-chunk-latency reconstruction. Each event carries `synthetic`: `true` means the event was fabricated by replaying a single non-streaming HTTP response as a one-shot event sequence (all landing within microseconds of each other), not observed from a genuine live stream. This endpoint deliberately does not filter `synthetic` events out — callers decide (see docs/TELEMETRY_AND_DATABASE.md §4).

### Response
```json
{
  "request_id": "01921a4f-5b68-7182-938b-829d10e01234",
  "events": [
    { "attempt_number": 1, "timestamp": "2026-09-23T03:26:29.507Z", "sequence": 1, "chunk_type": "content", "byte_count": 5, "synthetic": false }
  ]
}
```

---

## 11. GET `/v1/analytics/providers/comparison`

Per-provider/model aggregate stats: total and successful attempt counts, average latency, average tokens/sec, and average TTFT. Every timing average excludes attempts where `synthetic_timing = true` (non-streaming reconstructions) so real streaming performance isn't diluted by fabricated near-zero latencies.

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
      "avg_tokens_per_sec": 38.7,
      "avg_ttft_ms": 210.4
    }
  ]
}
```

---

## 12. GET `/v1/analytics/fallbacks`

Fallback frequency and root cause: for each request that ultimately succeeded after at least one failed attempt, which provider(s) failed and why. Capped at the 200 most recent such requests.

### Response
```json
{
  "fallbacks": [
    {
      "request_id": "01921a4f-5b68-7182-938b-829d10e01234",
      "succeeded_provider": "openrouter",
      "failed_provider": "llama",
      "failed_model": "qwen2.5-coder:7b",
      "error_code": "server_error",
      "error_message": "upstream 503"
    }
  ]
}
```

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
