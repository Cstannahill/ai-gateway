# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AI Gateway is a personal, high-performance Rust reverse proxy (Axum/Tokio) that unifies multiple LLM backends (llama.cpp, vLLM, Ollama/Ollama Cloud, OpenRouter, Gemini, Hugging Face, and any generic OpenAI-compatible endpoint) behind standard OpenAI-compatible HTTP endpoints, with first-class asynchronous telemetry persisted to PostgreSQL.

## Commands

The full list of commands, CLI flags, environment variables, and HTTP endpoints is in `COMMANDS.md` (its `--help` block must match `src/cli.rs::HELP`; a unit test enforces it — update both together).

```bash
# Build & run
cargo run --release                          # starts server (default port 8999)
cargo run --release -- path/to/config.yaml   # explicit config path (see config resolution order below)

# Compile-check (fast, no codegen)
cargo check --all-targets

# Full test suite
cargo test --all-targets

# Run a single test file / single test
cargo test --test routing_tests
cargo test --test api_tests -- test_health_endpoint

# Opt-in live provider smoke tests (skip silently unless env vars are set):
# see docs/PROVIDER_INTEGRATION_GUIDE.md "Live Smoke Tests"
cargo test --test live_provider_smoke_tests -- --nocapture --test-threads=1

# Opt-in test of the llama-server manager against the real binary (silent skip
# unless LIVE_LLAMA_SERVER_BINARY + LIVE_LLAMA_MODEL_GGUF are set; see its header):
cargo test --test live_llama_server_test -- --nocapture

# DB-dependent tests (reasoning-visibility ordering, snapshots, cost accounting,
# the analytics API, and one audit contract probe) degrade to a no-op unless a
# disposable Postgres is available — see Known limitations below.
docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16
AUDIT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/postgres cargo test --all-targets
```

Environment variables read at startup (see `docs/CONFIGURATION_GUIDE.md` for the full list): `DATABASE_URL`, `RUST_LOG`, and whatever each provider's `api_key_env` names (conventionally `OPENROUTER_API_KEY`, `GEMINI_API_KEY`, `HF_API_KEY`, `OLLAMA_API_KEY`). The binary loads `./.env` (current directory only) or `--env-file <PATH>` before logging starts; real environment variables win over the file. `.env.example` lists every variable; `COMMANDS.md` documents the flags.

Config resolution order: CLI arg path → `config/gateway.yaml` → `gateway.yaml` (cwd) → built-in defaults with a warning.

Toolchain: Rust edition 2024, requires Rust 1.88+ (let-chains); the README's stated "1.85+" is stale.

## Architecture

### Application icon
`AI-Gateway.png` (repo root) is the master artwork. `node scripts/generate-icons.mjs` crops it and regenerates every derivative: the frontend favicon/Apple/manifest icons and sidebar logo (`frontend/src/app/{icon.png,apple-icon.png,favicon.ico}`, `frontend/public/icons/`), and the backend's `assets/icon.ico` (embedded in `ai-gateway.exe` by `build.rs` + `assets/app.rc`, Windows only, a warning rather than an error if `rc.exe` is missing) and `assets/{favicon.ico,icon.png}` (served by the gateway at `/favicon.ico` and `/icon.png`, `src/server/routes/assets.rs`). To change the icon, replace `AI-Gateway.png`, rerun the script, and commit the outputs.

### Module map (`src/`)
- `cli.rs` — hand-rolled argument parser and the `--help` text (`HELP`); the binary takes `[CONFIG]`/`-c`, `-h`, `-V` only.
- `config/` — YAML config loading/validation (`GatewayConfig`, `ServerConfig`, `ProviderConfig`, model definitions).
- `types/` — canonical internal domain models: `GatewayRequest`/`GatewayResponse` (request.rs/response.rs), `ModelDefinition`/`ModelCapabilities` (model.rs), `ProviderType`/`ProviderHealth` (provider.rs), `ReasoningOutput` (reasoning.rs).
- `registry/` — `ModelRegistry` (alias → candidate array resolution) and `ProviderRegistry` (live adapter instances, health, lock-free `DashMap` state).
- `routing/` — the routing pipeline (`mod.rs`: `Router`, `RoutingPlan`, `RoutingDecision`), fallback/retry predicates (`fallback.rs`), and policy implementations (`policy.rs`: priority/round-robin/lowest-latency).
- `providers/` — one `Provider` trait (`mod.rs`) implemented per backend. Almost all adapters wrap `openai_compatible.rs::OpenAICompatibleAdapter` and only override request enrichment for vendor-specific params (`provider_options` escape hatch, e.g. `min_p`/`repeat_penalty` for llama.cpp, `guided_json` for vLLM, `thinkingConfig` for Gemini).
- `server/` — Axum router/middleware wiring (`mod.rs::build_router`, `AppState`) and route handlers under `routes/` (`chat_completions.rs` — also serves `/v1/completions`, `responses.rs`, `embeddings.rs`, `models.rs`, `health.rs`, `metrics.rs`, `providers.rs`, `analytics.rs`).
- `telemetry/` — async, non-blocking observability pipeline: `mod.rs` defines `TelemetryEvent` and the `mpsc` channel, `stream_instrumentation.rs::InstrumentedStream` wraps provider streams to derive TTFT/TPS/etc. (using a monotonic clock for attempt duration, and tagging every timing-sensitive event `synthetic: true` when it was reconstructed from a non-streaming response rather than observed live), `writer.rs::TelemetryWriter` is the background batch writer to PostgreSQL (runs via `sqlx::migrate!("./migrations")` at startup).
- `server/routes/analytics.rs` — read-only `/v1/analytics/*` query API over the telemetry schema (request list/detail, chunk timeline, per-provider comparison, fallback analysis), backed by `AppState.analytics_pool` (a separate `sqlx::PgPool` from the telemetry writer's own; `None`, and every endpoint returns `503`, if no database is configured/reachable).
- `llama_server/` — optional (off by default) manager for a local `llama-server` process, exposed at `/llama-server/*` (`server/routes/llama_server.rs`): saved models (JSON file), Windows-safe arg tokenizer, a supervisor that spawns/stops the child and tracks `starting/loading/ready/exited/failed`, log ring buffer, and `/health`/`/props` probing. It is the only feature that executes a local binary, so it is guarded (loopback-only bind required, browser-`Origin`/`Host` guard on every route, fixed `binary`, reserved flags rejected). UI contract for the frontend agent: `docs/frontend/LLAMA_SERVER_UI.md`. The linked provider (`llama_server.provider`, default `llama`) is wrapped by `providers/llama_managed.rs::ManagedLlamaProvider`: saved models are listed under it, and a request naming a saved model (alias or `.gguf` path) swaps to it automatically (`auto_swap`), draining in-flight requests first via an `RwLock` gate held inside response streams (`tests/llama_provider_tests.rs`). Tests use `examples/fake_llama_server.rs` (`tests/llama_server_tests.rs`).

### Request lifecycle (why files span multiple modules)
1. Axum middleware stack (trace → CORS → UUIDv7 request-id → body limit) → route handler deserializes into canonical `GatewayRequest`.
2. `ModelRegistry::resolve` turns the `model` field into candidates: a string containing `/` (e.g. `openrouter/anthropic/claude-3.5-sonnet`) is **explicit addressing** (single candidate, bypasses alias/priority machinery); otherwise it's a **logical alias** (e.g. `coding-fast`) that expands to a priority-ordered candidate array from `config/gateway.yaml`.
3. `Router` runs a 6-stage pipeline (explicit short-circuit → resolve → availability filter via `ProviderRegistry::is_available` → capability filter (vision/tools/context) → policy ordering → `RoutingDecision` emitted to telemetry).
4. The gateway iterates candidates, dispatching through the matched `Provider::execute`. Failures are classified via `routing::fallback::should_fallback` against `routing.fallback_on` (timeout/rate_limit/unavailable/server_error/content_filter): retryable errors under `max_retries_per_provider` retry the *same* provider with exponential backoff; otherwise the gateway moves to the *next* candidate.
5. Streaming responses are wrapped in `InstrumentedStream`, which both re-serializes provider events into OpenAI SSE chunks for the client and pushes `TelemetryEvent`s into a bounded `mpsc` channel — telemetry never blocks the client response path.
6. `TelemetryWriter` batches events (flush on 1000ms tick or 100-item buffer) and writes them, in one transaction per flush with retry + JSONL fallback on repeated failure, across the 13-table PostgreSQL schema (`migrations/001_initial_schema.sql` plus three follow-on migrations — see `migrations/`). If PostgreSQL is unreachable at startup, telemetry degrades to log-only rather than failing the server.

### Adding a new provider
Implement the `Provider` trait (`src/providers/mod.rs`), normally by wrapping `OpenAICompatibleAdapter` and overriding only the parts that differ (endpoint construction, `provider_options` namespace handling, capability defaults). Then add a `ProviderType` variant (`src/types/provider.rs`), export the module (`src/providers/mod.rs`), and add the instantiation arm in `src/main.rs`. Full walkthrough with code in `docs/PROVIDER_INTEGRATION_GUIDE.md`.

### Dual addressing & escape hatches
- Logical aliases decouple client tools from specific providers/models and get automatic failover; explicit `provider/model` strings guarantee exact reproducible execution and skip the alias/priority machinery.
- Standard OpenAI params are normalized into `GatewayRequest`; everything vendor-specific goes through the unconstrained `provider_options` map (keyed by provider name, e.g. `provider_options.llama_cpp`, `provider_options.vllm`, `provider_options.gemini`) so core types never get polluted with per-vendor fields. See `docs/PROVIDER_INTEGRATION_GUIDE.md` for the parameter tables per provider.

### Detailed docs (`docs/`)
- `ARCHITECTURE.md` — design principles, full request lifecycle diagram, concurrency/memory notes.
- `ROUTING_AND_POLICIES.md` — addressing modes, the 6-stage pipeline, fallback/retry trigger table.
- `TELEMETRY_AND_DATABASE.md` — the 12-table schema, streaming metric formulas (TTFT/TPS/jitter), example analytical SQL.
- `CONFIGURATION_GUIDE.md` — exhaustive `config/gateway.yaml` reference.
- `PROVIDER_INTEGRATION_GUIDE.md` — per-provider escape-hatch parameters and the "add a new adapter" steps.
- `API_REFERENCE.md` — HTTP endpoint specs with example requests/responses.

## Known limitations — read before trusting docs or making telemetry/correctness claims

`docs/INIT_CONVERSATION_AUDIT.md` (Codex, findings A01–A18) and `docs/INIT_CONVERSATION_AUDIT_CLAUDE.md` (Claude, findings CL01–CL08) are the two original audits of gaps between this project's docs/README and its actual runtime behavior as of 2026-09-21. `docs/REMEDIATION_PLAN.md` is the authoritative, continuously-updated status of what's been fixed since — its "Implementation status" blocks at the top (newest first) are the ground truth, not the audits themselves, which are frozen historical snapshots.

As of the most recent status block: **Phases 0–5 are done** (Phase 5's actionable items: durable outbox, circuit breaker, rate limiting, health scoring, `max_concurrent`, real `cheapest`/`local_first`/`highest_quality`/`balanced` routing policies, exact-match response caching). Explicitly not built, by decision: OTLP export (declined — Postgres is the sole system of record), semantic caching, the dataset/SFT-DPO workflows (need the Python layer), auth (personal loopback-only deployment), and learned/AI-driven routing (must never be built — deterministic routing only). `docs/frontend/API_GAPS.md`'s backlog is fully closed. Telemetry-trustworthiness fixes and the analytics API were verified against a real disposable Postgres, not just by code review — see `tests/db_verification_tests.rs` and `tests/analytics_tests.rs`. The llama-server manager (above) was added afterwards and verified against a fake server and the real binary. The Next.js dashboard lives in `frontend/` (owned by a separate agent); the Python analysis companion does not exist.

Most `tests/*.rs` files need no database and run identically in any environment. A handful (`tests/db_verification_tests.rs`, `tests/analytics_tests.rs`, and one probe in `tests/audit_contract_tests.rs`) need a real Postgres reachable via `AUDIT_DATABASE_URL`; they degrade to a graceful no-op (not a failure) when that variable is unset, so `cargo test --all-targets` is green either way — set the variable to actually exercise them (see the Commands section above).

Treat statements in `docs/*.md` about anything not yet reflected in `docs/REMEDIATION_PLAN.md`'s latest status block as design intent, not verified current behavior — check that file first before relying on a claim elsewhere.
