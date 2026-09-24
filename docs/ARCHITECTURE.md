# System Architecture & Design Principles

This document provides a comprehensive technical overview of the **AI Gateway** architecture, design philosophy, and internal execution model.

---

## 1. Architectural Philosophy

The AI Gateway is engineered as a high-performance, personal inference hub designed to bridge heterogeneous LLM providers (local GPU inference, multi-provider cloud aggregators, and direct model hosts) while maintaining strict observability, low latency, and protocol compatibility.

### Core Principles
1. **Decoupled Canonical Domain**:
   OpenAI's HTTP schema is treated solely as an external interface protocol, **never** as the internal domain model. Inbound requests are parsed into a normalized internal structure ([`GatewayRequest`](file:///s:/Code/RustProjects/AI-Gateway/src/types/request.rs#L12-L115)) and normalized outbound into [`GatewayResponse`](file:///s:/Code/RustProjects/AI-Gateway/src/types/response.rs#L14-L45) or streaming [`ProviderEvent`](file:///s:/Code/RustProjects/AI-Gateway/src/providers/events.rs#L20-L124) sequences.
2. **80/20 Schema Normalization + Escape Hatches**:
   Standard parameters (model, messages, temperature, top_p, tools, structured output, reasoning) are fully normalized. Provider-specific features (`min_p`, `repeat_penalty`, `guided_json`, `thinkingConfig`, routing hints) are routed via the unconstrained [`provider_options`](file:///s:/Code/RustProjects/AI-Gateway/src/types/request.rs#L105) escape hatch map without corrupting core data types.
3. **First-Class Telemetry with Zero Pipeline Stalls**:
   Telemetry recording is entirely decoupled from client request serving. Workers communicate via bounded, non-blocking asynchronous channels ([`tokio::sync::mpsc`](file:///s:/Code/RustProjects/AI-Gateway/src/telemetry/mod.rs#L30)) to a dedicated background batch writer ([`TelemetryWriter`](file:///s:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs#L50-L65)), guaranteeing zero request-path latency penalty.
4. **Time-Ordered Identifiers**:
   All entities (requests, attempts, events) utilize **UUIDv7** identifiers, ensuring monotonic time ordering and index clustering in PostgreSQL.

---

## 2. High-Level System Architecture

```mermaid
graph TD
    Client[Client Applications / Tools] -->|HTTP / SSE| Server[Axum HTTP Server]
    
    subgraph Server Middleware Stack
        Server --> Trace[TraceLayer]
        Trace --> Cors[CorsLayer]
        Cors --> ReqId[RequestIdLayer UUIDv7]
        ReqId --> BodyLimit[DefaultBodyLimit]
    end

    BodyLimit --> Router[Server Route Handlers]

    subgraph Route Handlers
        Router --> Chat[/v1/chat/completions]
        Router --> Responses[/v1/responses]
        Router --> Embeddings[/v1/embeddings]
        Router --> Models[/v1/models]
        Router --> Health[/health]
        Router --> Metrics[/metrics]
        Router --> Providers[/providers]
    end

    Chat --> ParseReq[Parse to Canonical GatewayRequest]
    Responses --> ParseReq

    subgraph Core Routing Engine
        ParseReq --> ModelResolver[Model Registry Alias & ID Resolver]
        ModelResolver --> CandidateFilter[Capability & Health Filter]
        CandidateFilter --> PolicyEngine[Routing Policy: Priority / RoundRobin / Latency]
        PolicyEngine --> RoutingPlan[Ordered Candidate Deployments]
    end

    RoutingPlan --> ExecLoop[Execution & Fallback Retry Loop]

    subgraph Provider Adapters
        ExecLoop -->|Candidate 1| LlamaCpp[llama.cpp / llama-server]
        ExecLoop -->|Candidate 2| OpenRouter[OpenRouter Cloud]
        ExecLoop -->|Candidate 3| VLLM[vLLM Inference]
        ExecLoop -->|Candidate 4| Gemini[Google Gemini AI]
        ExecLoop -->|Candidate 5| Ollama[Ollama / Ollama Cloud]
        ExecLoop -->|Candidate 6| HF[Hugging Face Inference]
    end

    ExecLoop --> StreamInst[InstrumentedStream Wrapper]
    StreamInst -->|SSE Event Stream| Client
    StreamInst -->|Async Telemetry Events| TelemChannel[(mpsc::Sender TelemetryEvent)]

    subgraph Telemetry Pipeline
        TelemChannel --> BatchBuffer[TelemetryWriter In-Memory Buffer]
        BatchBuffer -->|Batch Flush Every 1000ms / 100 items| Postgres[(PostgreSQL 12 Tables)]
    end
```

---

## 3. Module Hierarchy & Responsibilities

The codebase is organized into modular subsystems under [`src/`](file:///s:/Code/RustProjects/AI-Gateway/src):

```text
src/
├── lib.rs                   # Library crate root (exports all subsystems)
├── main.rs                  # CLI entry point, signal handling, server daemon
├── config/                  # YAML configuration loading & validation
│   └── mod.rs               # GatewayConfig, ServerConfig, ProviderConfig, etc.
├── types/                   # Canonical internal domain models
│   ├── mod.rs               # Module exports
│   ├── model.rs             # ModelDefinition, ModelCandidate, ModelCapabilities
│   ├── provider.rs          # ProviderType, ProviderHealth, ProviderStatus
│   ├── reasoning.rs         # ReasoningVisibility, ReasoningOutput
│   ├── request.rs           # GatewayRequest, Message, Tool, ContentPart
│   └── response.rs          # GatewayResponse, Choice, Usage
├── registry/                # Dynamic and static catalog registries
│   ├── mod.rs               # Module exports
│   ├── model_registry.rs    # Resolves model aliases to candidate arrays
│   └── provider_registry.rs # Runtime adapter instances, health, active metrics
├── routing/                 # Core routing engine & decision trees
│   ├── mod.rs               # Router, RoutingPlan, RoutingDecision
│   ├── fallback.rs          # FallbackTrigger, RetryConfig, should_fallback
│   └── policy.rs            # RoutingPolicy implementations
├── providers/               # Trait-based provider adapter subsystem
│   ├── mod.rs               # Provider trait & ProviderOutput enum
│   ├── events.rs            # ProviderEvent (streaming lifecycle event stream)
│   ├── openai_compatible.rs # Base adapter with header/body normalization
│   ├── llama_cpp.rs         # llama.cpp adapter with sampling enrichment
│   ├── vllm.rs              # vLLM adapter with guided decoding enrichment
│   ├── openrouter.rs        # OpenRouter aggregator adapter
│   ├── gemini.rs            # Google Gemini AI adapter
│   ├── ollama.rs            # Ollama local daemon adapter
│   ├── ollama_cloud.rs      # Ollama Cloud adapter
│   └── huggingface.rs       # Hugging Face Inference API adapter
├── server/                  # Axum HTTP server & REST/SSE endpoints
│   ├── mod.rs               # build_router, AppState, middleware wiring
│   ├── middleware.rs        # UuidV7RequestId generator
│   └── routes/              # HTTP Route handlers
│       ├── mod.rs           # Route tree definitions
│       ├── chat_completions.rs # POST /v1/chat/completions (SSE & non-stream)
│       ├── responses.rs     # POST /v1/responses (Responses API)
│       ├── embeddings.rs    # POST /v1/embeddings
│       ├── models.rs        # GET /v1/models (Dynamic & static merge)
│       ├── health.rs        # GET /health
│       ├── metrics.rs       # GET /metrics (Live stats)
│       └── providers.rs     # GET /providers (Runtime provider states)
└── telemetry/               # Observability & persistence subsystem
    ├── mod.rs               # TelemetryPipeline, TelemetryEvent enum
    ├── stream_instrumentation.rs # InstrumentedStream (TTFT, TPS, metrics)
    └── writer.rs            # TelemetryWriter (sqlx PostgreSQL batch writer)
```

---

## 4. The Request Execution Lifecycle

### Step 1: Inbound Processing & Context Injection
1. Client connects via HTTP/1.1 or HTTP/2.
2. `TraceLayer` creates a tracing span.
3. `SetRequestIdLayer` assigns a fresh **UUIDv7** request ID.
4. `PropagateRequestIdLayer` ensures `x-request-id` header is attached to client response.
5. Inbound JSON is deserialized into protocol structures and mapped to [`GatewayRequest`](file:///s:/Code/RustProjects/AI-Gateway/src/types/request.rs#L12).
6. Gateway records `TelemetryEvent::RequestReceived`.

### Step 2: Model Resolution & Candidate Generation
1. Router queries [`ModelRegistry::resolve`](file:///s:/Code/RustProjects/AI-Gateway/src/registry/model_registry.rs#L32).
   - If model matches an alias (e.g. `coding-fast`), registry returns all configured deployments with priorities.
   - If model contains a slash (e.g. `openrouter/anthropic/claude-3.5-sonnet`), registry creates a single explicit deployment candidate.
2. Router verifies candidate health against [`ProviderRegistry::is_available`](file:///s:/Code/RustProjects/AI-Gateway/src/registry/provider_registry.rs#L82). Unhealthy or disabled providers are excluded, as are providers at their configured `max_concurrent` ceiling or with a tripped circuit breaker (`ProviderRegistry::is_at_capacity`/`is_circuit_open` — see `docs/ROUTING_AND_POLICIES.md` for the breaker's Closed/Open/HalfOpen state machine, driven by consecutive attempt failures recorded in Step 3 below). Both checks also apply to explicit `provider/model` addressing, which bypasses capability/priority filtering but not these resource-protection constraints.
3. Router matches capabilities (e.g. rejecting candidates lacking vision support if images are in request).
4. Router orders remaining candidates according to the active policy (e.g. priority, round-robin, latency).
5. Router emits `TelemetryEvent::RoutingDecisionRecorded` logging full reasoning.

### Step 3: Provider Attempt & Fallback Loop
1. The gateway iterates through candidates in order up to `max_retries_per_provider`.
2. Provider adapter enriches the request with vendor-specific parameters (`provider_options`).
3. Adapter dispatches HTTP request with `attempt_timeout_secs` enforcement.
4. **On Success**:
   - For **Streaming**: Adapter returns `ProviderOutput::Stream`. Stream is wrapped in [`InstrumentedStream`](file:///s:/Code/RustProjects/AI-Gateway/src/telemetry/stream_instrumentation.rs#L19). Each chunk is converted to an OpenAI SSE chunk, terminating with `data: [DONE]\n\n`.
   - For **Non-Streaming**: Stream is consumed in-process, accumulating content, reasoning thoughts, and tool call parts into a complete `GatewayResponse`.
5. **On Failure (Timeout / 429 / 5xx / Connection Error)**:
   - Gateway records attempt telemetry with status code and error details.
   - Gateway checks [`fallback::should_fallback`](file:///s:/Code/RustProjects/AI-Gateway/src/routing/fallback.rs#L104).
   - If fallback triggers, gateway proceeds immediately to the next candidate.

### Step 4: Telemetry Ingestion & Batch Flush
1. Events generated during execution are passed into `mpsc::Sender<TelemetryEvent>`.
2. The background `TelemetryWriter` worker loops:
   - Buffers incoming events into in-memory vectors.
   - Flushes on timer tick (default 1000ms) or when buffer reaches capacity (default 100 items).
   - Executes the whole batch as one transaction, retrying with backoff on failure.
   - If Postgres is still unreachable after retries, the batch is journaled to a local JSONL fallback log instead of being dropped. That log is replayed automatically — on `TelemetryWriter` startup, and again on every subsequent flush tick — so an extended outage that later recovers drains itself without operator intervention or requiring a process restart (`src/telemetry/writer.rs::replay_fallback_log`, docs/REMEDIATION_PLAN.md Phase 5.1). This is not a full write-ahead-logged outbox: an event is only journaled after an in-memory batch has already failed its DB attempts, not before every write, so a hard process crash between receiving an event and a failed flush can still lose it.
   - Every flush outcome, dropped-event count (channel-full `try_send` failures), and fallback-log state updates a shared `TelemetryHealth` handle (`src/telemetry/health.rs`), exposed live via `GET /v1/analytics/persistence` — distinct from `/health`'s provider-only view, and the only way to actually know whether telemetry is landing in the database right now (docs/frontend/API_GAPS.md item 8).

---

## 5. Concurrency & Memory Management

- **Zero Global Locks in Fast Path**: Active request tracking and provider states use lock-free [`DashMap`](file:///s:/Code/RustProjects/AI-Gateway/src/registry/provider_registry.rs#L20) structures.
- **Connection Pooling**: Upstream HTTP calls share persistent `reqwest::Client` connection pools per provider adapter.
- **Backpressure Protection**: Telemetry channels use bounded queues. If the database lags severely, events drop gracefully with warnings rather than exhausting server memory.
