# AI Gateway

<div align="center">

<img src="AI-Gateway.png" alt="AI Gateway" width="128">

**A high-performance personal LLM inference gateway with first-class telemetry, intelligent routing, and multi-provider failover.**

[![Language](https://img.shields.io/badge/language-Rust_2024-orange.svg)](https://www.rust-lang.org/)
[![Framework](https://img.shields.io/badge/framework-Axum_0.8-blue.svg)](https://github.com/tokio-rs/axum)
[![Database](https://img.shields.io/badge/database-PostgreSQL_16-blue.svg)](https://www.postgresql.org/)
[![Tests](https://img.shields.io/badge/tests-58%2F58_passed-brightgreen.svg)]()
[![Warnings](https://img.shields.io/badge/warnings-0-brightgreen.svg)]()

</div>

---

## Overview

Modern LLM workflows rarely depend on a single model or provider. A developer typically relies on a heterogeneous inference stack:
- **Local GPUs**: `llama-server` (llama.cpp) or `vLLM` for fast, private, zero-marginal-cost coding and completions.
- **Local Services**: `Ollama` for simple command-line model management.
- **Cloud Aggregators**: `OpenRouter` for cost-effective fallback to hundreds of open-weights and proprietary models.
- **Direct Frontier APIs**: `Google Gemini`, `Anthropic`, `OpenAI`, or `Hugging Face`.

**AI Gateway** is a personal, low-latency reverse proxy written in Rust that unifies all these backends behind standard OpenAI-compatible endpoints (`/v1/chat/completions`, `/v1/responses`, `/v1/embeddings`, `/v1/models`).

Instead of hardcoding provider endpoints and API keys into dozens of local tools, editors, and scripts, point all applications to:
```text
http://localhost:8999/v1
```

```mermaid
graph TD
    Client[Your Tools & Applications<br/>Cursor, Cline, Aider, LangChain, cURL] -->|OpenAI Protocol| Gateway[AI Gateway :8999]

    subgraph "AI Gateway Core Engine"
        Gateway --> Router[Routing & Policy Engine]
        Router --> Registry[Model & Provider Registry]
        Router --> Fallback[Retry & Fallback Manager]
        Gateway --> StreamInst[Zero-Cost Streaming Instrumentation]
        StreamInst --> TelemQueue[Non-Blocking Bounded Channel]
    end

    Router --> Llama[llama-server / llama.cpp]
    Router --> OpenRouter[OpenRouter Cloud]
    Router --> Vllm[vLLM Inference]
    Router --> Ollama[Ollama / Ollama Cloud]
    Router --> Gemini[Google Gemini AI]
    Router --> HF[Hugging Face API]

    TelemQueue --> BatchWriter[Background Batch Writer]
    BatchWriter --> Postgres[(PostgreSQL Database<br/>12 Relational Tables)]
```

---

## Key Features

- **Protocol Compatibility**: Full drop-in replacement for OpenAI endpoints (`/v1/chat/completions`, `/v1/responses`, `/v1/embeddings`, `/v1/models`).
- **Standard SSE Streaming**: Envelopes each chunk with standard OpenAI root fields (`id`, `object`, `created`, `model`, `choices`), terminates properly with `data: [DONE]`, and calculates streaming latencies without buffering delays.
- **Dual Addressing Modes**:
  - **Logical Aliases** (`coding-fast`, `coding-strong`): Decouples client tools from specific model versions, prioritizing local inference with automatic cloud failover.
  - **Explicit Addressing** (`openrouter/anthropic/claude-3.5-sonnet`): Guarantees exact model execution for reproducibility.
- **80/20 Schema Normalization + Escape Hatches**: Normalized core schema combined with unconstrained `provider_options` pass-throughs for vendor-specific parameters (`min_p`, `repeat_penalty`, `top_k`, `guided_json`, `thinkingConfig`).
- **Non-Streaming Tool Call & Reasoning Accumulation**: Transparently reconstructs multi-chunk tool calls and reasoning outputs (`reasoning_content`) into single responses.
- **First-Class Relational Telemetry**: Asynchronous, non-blocking telemetry writes across **12 PostgreSQL tables**, tracking request parameters, routing reasons, candidate attempts, chunk timelines, token counts, and costs.
- **Log-Only Fallback**: Gracefully operates even if PostgreSQL is offline or unconfigured.
- **Active Health Monitoring**: Background worker continuously verifies provider readiness and latency every 30 seconds.
- **Production-Grade Middleware**: Mounted Tower HTTP middleware stack with CORS, tracing, monotonic UUIDv7 request ID propagation, and payload size limiting.

---

## Quickstart

### 1. Prerequisites
- **Rust Toolchain**: 1.88+ (Edition 2024; the codebase uses let-chain syntax stabilized in 1.88 — an older toolchain will fail to compile despite the Edition 2024 minimum implying 1.85).
- **PostgreSQL** *(Optional)*: Required for persistent telemetry and dataset generation.

### 2. Configuration
Copy or edit [`config/gateway.yaml`](file:///s:/Code/RustProjects/AI-Gateway/config/gateway.yaml):

```yaml
server:
  host: "0.0.0.0"
  port: 8999

providers:
  llama:
    name: "llama"
    type: "llama_cpp"
    base_url: "http://localhost:8080"
    timeout_secs: 300

  openrouter:
    name: "openrouter"
    type: "open_router"
    base_url: "https://openrouter.ai/api"
    api_key_env: "OPENROUTER_API_KEY"
    timeout_secs: 120

models:
  coding-fast:
    description: "Local coding with cloud fallback"
    candidates:
      - provider: "llama"
        model: "qwen3.5-9b"
        priority: 1
      - provider: "openrouter"
        model: "qwen/qwen-2.5-coder-32b-instruct"
        priority: 2
    capabilities:
      tools: true
      streaming: true

telemetry:
  enabled: true
  database_url_env: "DATABASE_URL"
```

### 3. Build & Run
```bash
# Set your environment variables
export DATABASE_URL="postgres://postgres:password@localhost:5432/ai_gateway"
export OPENROUTER_API_KEY="sk-or-v1-..."

# Run the server
cargo run --release
```

The gateway starts at `http://localhost:8999`.

### 4. Send Your First Request
```bash
curl -X POST http://localhost:8999/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding-fast",
    "messages": [
      {"role": "user", "content": "Write a quicksort in Rust"}
    ],
    "stream": true
  }'
```

---

## Project Structure

```text
ai-gateway/
├── Cargo.toml               # Package definition (Axum 0.8, Tokio, SQLx, Tower)
├── config/
│   └── gateway.yaml         # Server, provider, model, routing, & telemetry config
├── migrations/
│   └── 001_initial_schema.sql # 12-table PostgreSQL schema with UUIDv7
├── src/
│   ├── lib.rs               # Library root (exports all gateway subsystems)
│   ├── main.rs              # Application entry point & health check daemon
│   ├── config/              # Configuration parser & validation
│   ├── types/               # Canonical domain models (Request, Response, Model)
│   ├── registry/            # Model and Provider catalogs & runtime state
│   ├── routing/             # 6-stage routing pipeline & fallback manager
│   ├── providers/           # Adapter layer (llama.cpp, vLLM, OpenRouter, etc.)
│   ├── server/              # Axum HTTP routes, SSE generator, & middleware
│   └── telemetry/           # Asynchronous PostgreSQL batch persistence engine
├── tests/                   # Integration test suites
│   ├── api_tests.rs         # Endpoints verification (/health, /metrics, etc.)
│   ├── routing_tests.rs     # Capability, priority, & alias resolution tests
│   ├── fallback_tests.rs    # Error trigger & fallback policy tests
│   ├── provider_options_tests.rs # Vendor parameter enrichment tests
│   └── sse_tests.rs         # SSE chunk formatting & protocol tests
└── docs/                    # Detailed technical documentation
    ├── ARCHITECTURE.md
    ├── ROUTING_AND_POLICIES.md
    ├── TELEMETRY_AND_DATABASE.md
    ├── CONFIGURATION_GUIDE.md
    ├── PROVIDER_INTEGRATION_GUIDE.md
    └── API_REFERENCE.md
```

---

## Detailed Documentation Index

For in-depth guides, architectural references, and implementation specifics, explore the [`docs/`](file:///s:/Code/RustProjects/AI-Gateway/docs) directory:

1. [**System Architecture & Design Principles**](docs/ARCHITECTURE.md):
   Dual-layer architecture, canonical domain models, memory efficiency, and the complete request lifecycle.
2. [**Routing Engine & Policy Architecture**](docs/ROUTING_AND_POLICIES.md):
   Logical aliases vs explicit addressing, capability matching (vision/tools), routing policies, and fallback/retry rules.
3. [**Telemetry Architecture & Database Schema**](docs/TELEMETRY_AND_DATABASE.md):
   The 12 relational database tables, streaming metrics (TTFT, TPS, jitter), async batching pipeline, and dataset export queries.
4. [**Configuration Guide**](docs/CONFIGURATION_GUIDE.md):
   Exhaustive reference for every parameter in `config/gateway.yaml` with types, defaults, and examples.
5. [**Provider Integration & Escape Hatch Guide**](docs/PROVIDER_INTEGRATION_GUIDE.md):
   Configuration and parameter escape hatches for llama.cpp, OpenRouter, vLLM, Gemini, Ollama, and instructions for adding new adapters.
6. [**HTTP API Reference**](docs/API_REFERENCE.md):
   Full specification with cURL examples and JSON schemas for `/v1/chat/completions`, `/v1/responses`, `/v1/embeddings`, `/v1/models`, `/health`, `/metrics`, and `/providers`.

---

## Verification & Testing

The gateway includes an extensive automated integration test suite:

```bash
# Run all integration test suites across all targets
cargo test --all-targets

# Verify code compilation with zero warnings
cargo check --all-targets
```

### Test Results
- `unittests src/lib.rs`: **PASSED**
- `unittests src/main.rs`: **PASSED**
- `tests/api_tests.rs`: **PASSED** (6/6)
- `tests/audit_contract_tests.rs`: **PASSED** (14/14, 1 ignored — requires a live disposable PostgreSQL instance; see the file's own doc comment)
- `tests/bugfix_tests.rs`: **PASSED** (7/7)
- `tests/config_validation_tests.rs`: **PASSED** (12/12)
- `tests/fallback_tests.rs`: **PASSED** (3/3)
- `tests/provider_options_tests.rs`: **PASSED** (3/3)
- `tests/routing_tests.rs`: **PASSED** (6/6)
- `tests/sse_tests.rs`: **PASSED** (7/7)
- **Compiler Warnings**: **0 warnings** across all crates and tests.

See [`docs/INIT_CONVERSATION_AUDIT.md`](docs/INIT_CONVERSATION_AUDIT.md), [`docs/INIT_CONVERSATION_AUDIT_CLAUDE.md`](docs/INIT_CONVERSATION_AUDIT_CLAUDE.md), and [`docs/REMEDIATION_PLAN.md`](docs/REMEDIATION_PLAN.md) for a detailed, code-verified account of what is and isn't yet implemented — passing tests confirm the behavior they cover, not full API compatibility.
