# AI Gateway

AI Gateway is a personal Rust reverse proxy that presents multiple local and hosted language-model providers through OpenAI-compatible HTTP endpoints. It resolves model aliases, filters and orders provider candidates, retries eligible failures, and can record request and streaming telemetry asynchronously.

It is designed for a trusted, personal deployment—not as a multi-tenant service—and does not provide user authentication. The built-in server default binds to `0.0.0.0:8999` (all interfaces). Set `server.host: "127.0.0.1"` for local-only use, or place it behind a separately secured network boundary before exposing it to other machines.

## What it does

- Exposes chat completions, legacy completions, responses, embeddings, and model-list endpoints.
- Includes dedicated adapters for llama.cpp, vLLM, Ollama, Ollama Cloud, OpenRouter, Gemini, and Hugging Face, plus a generic OpenAI-compatible adapter. The configured xAI, Venice.ai, and Featherless.ai provider types currently use that generic adapter rather than dedicated implementations.
- Routes logical model aliases to ordered candidates; explicit `provider/model` addressing selects one configured provider/model directly.
- Retries eligible provider failures and can fail over to the next candidate. Streaming requests can hold the response until the first output so pre-output failures can fail over.
- Re-serializes provider streams as OpenAI-style server-sent events and emits telemetry through a bounded, non-blocking channel.
- Optionally writes telemetry to PostgreSQL. When a database is unavailable, the gateway can continue without persistent telemetry; analytics endpoints require a reachable database.
- Optionally manages a local `llama-server` process and saved models. This feature is disabled unless configured and, by default, requires a loopback gateway bind and applies browser-origin/host guards.
- Provides health, provider, gateway metrics, Prometheus, admin reload, analytics, and (when enabled) llama-server management endpoints.

## Requirements

- Rust 1.88 or newer (edition 2024).
- PostgreSQL only if persistent telemetry or the analytics API is needed.
- One or more inference providers configured in `config/gateway.yaml`.

## Quick start

1. Review [`config/gateway.yaml`](config/gateway.yaml) and add the providers and model aliases you intend to use. The checked-in file is an example, not a portable deployment configuration.
2. Set any provider API keys named by `api_key_env` and, if desired, `DATABASE_URL`. You can put local values in a `.env` file in the working directory; real environment variables take precedence. See [`.env.example`](.env.example).
3. Start the gateway:

   ```bash
   cargo run --release
   ```

   By default it listens on port `8999`. Configuration resolution is: CLI config path, `config/gateway.yaml`, `gateway.yaml`, then built-in defaults. To choose a config explicitly:

   ```bash
   cargo run --release -- path/to/config.yaml
   ```

4. Send a request using an alias defined in your configuration:

   ```bash
   curl http://127.0.0.1:8999/v1/chat/completions \
     -H 'Content-Type: application/json' \
     -d '{
       "model": "coding-fast",
       "messages": [{"role": "user", "content": "Explain Rust ownership briefly."}],
       "stream": false
     }'
   ```

Use `http://127.0.0.1:8999/v1` as the base URL in OpenAI-compatible clients. The alias above is present in the sample configuration; change it to an alias you have configured.

## API overview

| Endpoint | Purpose |
|---|---|
| `POST /v1/chat/completions` | Chat completions, including streaming |
| `POST /v1/completions` | Text completions |
| `POST /v1/responses` | Responses API |
| `POST /v1/embeddings` | Embeddings |
| `GET /v1/models` | Configured and provider-discovered model catalog |
| `GET /health` | Provider health summary |
| `GET /providers` | Provider status |
| `GET /metrics` | Gateway metrics |
| `GET /metrics/prometheus` | Prometheus exposition endpoint |
| `/v1/analytics/*` | Read-only telemetry queries; returns unavailable when no analytics database pool is configured |
| `/admin/reload` | Configuration reload status (`GET`) and reload (`POST`) |
| `/llama-server/*` | Optional local llama-server controls, available only when configured |

See [`docs/API_REFERENCE.md`](docs/API_REFERENCE.md) for request and response details and [`COMMANDS.md`](COMMANDS.md) for the full endpoint and command index.

## Web console

The repository also includes a Next.js console for chat, request and provider telemetry, model comparison, and optional llama-server management. It runs separately from the Rust gateway and connects through a same-origin server-side proxy; it does not need provider or database credentials.

With Node.js 22.18+ and pnpm 10.11+:

```bash
cd frontend
pnpm install --frozen-lockfile
cp .env.example .env.local
pnpm dev
```

Open `http://127.0.0.1:3000`. The default `GATEWAY_URL` is `http://127.0.0.1:8999`; see [`frontend/README.md`](frontend/README.md) for setup, console behavior, and frontend verification commands.

## Configuration and routing

Provider definitions specify adapter type, base URL, timeout, and—when needed—the environment variable containing the provider key. Model aliases contain one or more provider/model candidates with priorities and declared capabilities. The router applies availability and capability checks before ordering candidates according to the configured routing policy.

For example, a logical alias can prefer a local provider and list a hosted provider as a later candidate. Explicit addressing such as `openrouter/vendor/model-name` bypasses alias candidate selection and targets that provider/model. Retry limits, eligible fallback errors, stream preflight, and other settings are documented in [`docs/CONFIGURATION_GUIDE.md`](docs/CONFIGURATION_GUIDE.md) and [`docs/ROUTING_AND_POLICIES.md`](docs/ROUTING_AND_POLICIES.md).

Configuration and `.env` changes can be reloaded through the admin endpoint when reload support is active. Some settings—including the server bind, telemetry, pricing, caching, and llama-server manager—require a restart. See the configuration guide before relying on hot reload for a particular setting.

## Telemetry and optional features

Telemetry is sent to a bounded queue so database writes do not block the response path. The background writer batches PostgreSQL writes and has a JSONL fallback for persistent-write failures. PostgreSQL-backed analytics are read-only; with no database available at startup, the analytics routes return `503`.

The response cache is optional and disabled by default. It caches exact matches for non-streaming chat completions. The optional llama-server manager can scan and save local models, start/stop the process, and switch the linked provider to a requested saved model; enabling it executes a local binary, so review its security and loopback requirements in the configuration guide.

## Development and tests

```bash
cargo check --all-targets
cargo test --all-targets
```

Most tests do not require PostgreSQL. A small set of database verification and analytics tests only exercise their database paths when `AUDIT_DATABASE_URL` points to a disposable PostgreSQL database; otherwise they degrade to a no-op. To run those database paths:

```bash
AUDIT_DATABASE_URL="$YOUR_DISPOSABLE_POSTGRES_URL" cargo test --all-targets
```

Opt-in provider smoke tests and the real llama-server integration test have separate environment requirements; see [`docs/PROVIDER_INTEGRATION_GUIDE.md`](docs/PROVIDER_INTEGRATION_GUIDE.md) and the headers of the relevant tests.

## Project map

```text
src/
├── config/       YAML loading, validation, and environment overlay
├── types/        Canonical request, response, model, and provider types
├── registry/     Model aliases and live provider state
├── routing/      Candidate selection, policies, retries, and failover
├── providers/    Provider trait and backend adapters
├── server/       Axum routes, middleware, and application state
├── telemetry/    Event instrumentation and asynchronous persistence
└── llama_server/ Optional local process manager
frontend/          Next.js console
migrations/       PostgreSQL schema migrations
tests/            Unit and integration test targets
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Routing and policies](docs/ROUTING_AND_POLICIES.md)
- [Telemetry and database](docs/TELEMETRY_AND_DATABASE.md)
- [Configuration](docs/CONFIGURATION_GUIDE.md)
- [Provider integration](docs/PROVIDER_INTEGRATION_GUIDE.md)
- [HTTP API reference](docs/API_REFERENCE.md)
- [Commands, environment variables, and endpoint index](COMMANDS.md)
- [Remediation status and verified limitations](docs/REMEDIATION_PLAN.md)

The audit documents are historical snapshots. For current implementation status, consult the newest status block in [`docs/REMEDIATION_PLAN.md`](docs/REMEDIATION_PLAN.md).
