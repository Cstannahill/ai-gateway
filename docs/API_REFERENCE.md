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
