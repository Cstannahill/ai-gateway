# COMMANDS

Every command, flag, argument, and environment variable for AI Gateway: the
`ai-gateway` binary, the `cargo` development commands, the opt-in live tests, the
frontend scripts, and an index of the HTTP endpoints (the "commands" you send a
running gateway). For config-file fields see
[`docs/CONFIGURATION_GUIDE.md`](docs/CONFIGURATION_GUIDE.md); for full HTTP
payloads see [`docs/API_REFERENCE.md`](docs/API_REFERENCE.md).

## Contents

1. [The `ai-gateway` binary](#1-the-ai-gateway-binary)
2. [Environment variables](#2-environment-variables)
3. [Development commands (`cargo`)](#3-development-commands-cargo)
4. [Opt-in live tests](#4-opt-in-live-tests)
5. [Frontend commands](#5-frontend-commands)
6. [HTTP endpoint index](#6-http-endpoint-index)
7. [llama-server management](#7-llama-server-management)

---

## 1. The `ai-gateway` binary

```bash
cargo run --release                          # start with default config resolution
cargo run --release -- path/to/config.yaml   # explicit config file
cargo run --release -- --help                # print usage
./target/release/ai-gateway --version        # built binary (ai-gateway.exe on Windows)
```

There are no subcommands. The binary starts the server and runs until it
receives Ctrl+C (graceful shutdown: in-flight requests drain for
`server.shutdown_timeout_secs`, then a managed llama-server is stopped, then
telemetry is flushed).

| Argument | Meaning |
|---|---|
| `[CONFIG]` | Path to a YAML config file (positional). Same as `--config`. |
| `-c, --config <PATH>` | Path to a YAML config file. `--config=<PATH>` also works. |
| `--env-file <PATH>` | Load environment variables from this file instead of `./.env`. Must exist. `--env-file=<PATH>` also works. |
| `-h, --help` | Print usage and exit `0`. Wins over any other argument. |
| `-V, --version` | Print `ai-gateway <version>` and exit `0`. |

Only one config path and one env file may be given. Unknown flags, a missing
value, an empty path, or duplicates print an error to stderr and exit with
status `2`. An
explicit path that does not exist or fails validation exits `1` with the reason
(it does **not** fall back to another file).

### Config resolution order

1. `--config <PATH>` / `[CONFIG]`
2. `config/gateway.yaml`
3. `gateway.yaml` (current directory)
4. Built-in defaults (logs a warning)

### `--help` output

The text below is exactly what `ai-gateway --help` prints. It lives in
`src/cli.rs` (`HELP`), and a unit test fails if this block and the binary
disagree.

```text
ai-gateway - personal LLM gateway (OpenAI-compatible) with PostgreSQL telemetry

USAGE:
    ai-gateway [OPTIONS] [CONFIG]

ARGS:
    [CONFIG]    Path to a YAML config file. Same as --config.

OPTIONS:
    -c, --config <PATH>    Path to a YAML config file
        --env-file <PATH>  Load environment variables from this file (must exist)
    -h, --help             Print this help and exit
    -V, --version          Print the version and exit

CONFIG RESOLUTION (first match wins):
    1. --config <PATH> / [CONFIG]   (an explicit path that fails to load is an error)
    2. config/gateway.yaml
    3. gateway.yaml
    4. built-in defaults (logs a warning)

ENVIRONMENT:
    RUST_LOG              Log filter, e.g. ai_gateway=debug (default: ai_gateway=info,tower_http=info)
    DATABASE_URL          PostgreSQL URL for telemetry (name set by telemetry.database_url_env)
    <api_key_env>         One variable per provider, named in its api_key_env config field,
                          e.g. OPENROUTER_API_KEY, GEMINI_API_KEY, HF_API_KEY, OLLAMA_API_KEY
    Variables are also read from ./.env if it exists, or from --env-file. A variable
    already set in the real environment always wins over the file.

The gateway listens on server.host:server.port (default 0.0.0.0:8999). Stop it
with Ctrl+C for a graceful shutdown. HTTP endpoints, config fields and the
llama-server module are documented in COMMANDS.md and docs/.
```

---

## 2. Environment variables

The binary loads a dotenv file at startup, before logging and config are set up:

- `./.env` in the **current directory** is loaded if it exists (parent
  directories are never searched, and a missing file is fine).
- `--env-file <PATH>` loads that file instead; it must exist and parse, or the
  gateway exits `2`.
- A variable already set in the real environment **always wins** over the file,
  so `$env:RUST_LOG='debug'` still overrides `.env`.
  If the two values differ the gateway logs a warning at startup naming the
  variable (never the value), because a stale key exported by a shell profile
  or the system settings otherwise hides the correct one in `.env` with no
  visible sign. Check with `Get-ChildItem Env:OPENROUTER_API_KEY` (PowerShell)
  and look in `$PROFILE`.
- The startup log says which file was loaded (never the values).
- Syntax: `KEY=value`, `export KEY=value`, `#` comments, and quoted values
  (`KEY="has spaces"`). Copy [`.env.example`](.env.example) to `.env` to start.

Because the working directory decides which `.env` is found, start the gateway
from the repo root (or pass `--env-file`).

| Variable | Read by | Purpose |
|---|---|---|
| `RUST_LOG` | binary | `tracing` filter, e.g. `ai_gateway=debug,tower_http=info`. Default `ai_gateway=info,tower_http=info`. |
| `DATABASE_URL` | binary | PostgreSQL URL for telemetry and the analytics API. The variable *name* is configurable via `telemetry.database_url_env`; `telemetry.database_url` in YAML takes precedence. With no database, telemetry degrades to log-only and `/v1/analytics/*` returns `503`. |
| `OPENROUTER_API_KEY` | provider config | Conventional name for an OpenRouter key. |
| `GEMINI_API_KEY` | provider config | Conventional name for a Gemini key. |
| `HF_API_KEY` | provider config | Conventional name for a Hugging Face token. |
| `OLLAMA_API_KEY` | provider config | Conventional name for an Ollama Cloud key. |

The four `*_API_KEY` names are only conventions: a provider reads whichever
variable its own `api_key_env` field names (the sample `config/gateway.yaml`
uses exactly these four). A provider only exists if it is listed under
`providers:` in the config; a key in `.env` for an unlisted provider is
ignored. A missing or malformed key surfaces as an unhealthy provider, not a
crash.

**Several keys per provider.** A key variable may hold more than one key, comma-separated (`OPENROUTER_API_KEY=sk-a,sk-b`). Requests rotate round-robin across them; a key the provider refuses (`429`, `401`, `403`) is set aside for the `Retry-After` it gave (60 s if none, 10 min for 401/403) and the same request is retried at once on the next key. Only when every key is refused does the failure reach routing's retry/fallback. One key behaves exactly as before.

Test-only variables are listed in [section 4](#4-opt-in-live-tests); the
frontend's are in [section 5](#5-frontend-commands).

---

## 3. Development commands (`cargo`)

Toolchain: Rust edition 2024, Rust 1.88+.

| Command | What it does |
|---|---|
| `cargo build --release` | Optimized build (LTO, stripped) to `target/release/ai-gateway`. |
| `cargo run --release` | Build and start the server (default port `8999`). |
| `cargo check --all-targets` | Fast compile check of the library, binary, tests and examples. |
| `cargo test --all-targets` | Full test suite. DB-backed tests no-op unless `AUDIT_DATABASE_URL` is set. |
| `cargo test --all-targets --offline` | Same, without touching the network (uses the local registry cache). |
| `cargo test --test routing_tests` | One test file. |
| `cargo test --test api_tests -- test_health_endpoint` | One test in one file. |
| `cargo test --lib cli` | Only the CLI parser/help tests. |
| `cargo clippy --all-targets --offline` | Lint. The project keeps this warning-free. |
| `node scripts/generate-icons.mjs` | Regenerates every app icon (frontend favicon/manifest/sidebar logo, the exe icon, the gateway's `/favicon.ico`) from `AI-Gateway.png`. Needs `pnpm install` in `frontend/` first (it uses Next.js's bundled `sharp`). |
| `cargo build --example fake_llama_server` | Builds the fake llama-server used by `tests/llama_server_tests.rs` (the test builds it on demand; you rarely run this yourself). |

### With a database

DB-backed tests (telemetry ordering, snapshots, cost accounting, the analytics
API, one audit probe) need a disposable PostgreSQL:

```bash
docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16
AUDIT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/postgres cargo test --all-targets
```

PowerShell:

```powershell
$env:AUDIT_DATABASE_URL = 'postgres://postgres:postgres@127.0.0.1:55432/postgres'
cargo test --all-targets
```

---

## 4. Opt-in live tests

These talk to real backends and print `SKIP ...` (passing) unless their trigger
variable is set, so plain `cargo test` never needs them.

### Per-provider smoke tests

One tiny chat request plus one streaming request per provider, built through the
same `providers::build_adapter` path as `main.rs`.

```bash
LIVE_LLAMA_BASE_URL=http://localhost:8080 \
OPENROUTER_API_KEY=sk-or-... \
cargo test --test live_provider_smoke_tests -- --nocapture --test-threads=1
```

| Provider | Trigger | Other variables |
|---|---|---|
| llama.cpp | `LIVE_LLAMA_BASE_URL` | `LIVE_LLAMA_MODEL` (default `local`) |
| Ollama | `LIVE_OLLAMA_BASE_URL` | `LIVE_OLLAMA_MODEL` (required) |
| vLLM | `LIVE_VLLM_BASE_URL` | `LIVE_VLLM_MODEL` (required) |
| OpenRouter | `OPENROUTER_API_KEY` | `LIVE_OPENROUTER_MODEL` (default `openai/gpt-4o-mini`) |
| Gemini | `GEMINI_API_KEY` | `LIVE_GEMINI_MODEL` (default `gemini-2.0-flash`) |
| Hugging Face | `HF_API_KEY` | `LIVE_HF_MODEL` (required) |
| Ollama Cloud | `OLLAMA_API_KEY` | `LIVE_OLLAMA_CLOUD_MODEL` (required), `LIVE_OLLAMA_CLOUD_BASE_URL` (default `https://ollama.com`) |
| Generic OpenAI-compatible | `LIVE_GENERIC_BASE_URL` | `LIVE_GENERIC_MODEL` (required), `LIVE_GENERIC_API_KEY` (optional) |

Cloud targets cap `max_tokens` at 64 (a fraction of a cent per run). Details:
[`docs/PROVIDER_INTEGRATION_GUIDE.md`](docs/PROVIDER_INTEGRATION_GUIDE.md).

### Real llama-server manager test

Starts, inspects, and stops the real binary through the manager.

```powershell
$env:LIVE_LLAMA_SERVER_BINARY = 'S:\llama.cpp\build\bin\Release\llama-server.exe'
$env:LIVE_LLAMA_MODEL_GGUF    = 'S:\path\to\small-model.gguf'
cargo test --test live_llama_server_test -- --nocapture
```

| Variable | Required | Meaning |
|---|---|---|
| `LIVE_LLAMA_SERVER_BINARY` | yes | Path to `llama-server(.exe)`. Not a `.cmd` shim. |
| `LIVE_LLAMA_MODEL_GGUF` | yes | Path to a (small) `.gguf`. |
| `LIVE_LLAMA_MMPROJ` | no | Matching mmproj; also checks vision is reported. |
| `LIVE_LLAMA_ARGS` | no | Extra llama-server args (default `-ngl 0 -c 512`, CPU only). |

---

## 5. Frontend commands

The Next.js dashboard lives in `frontend/` (run from that directory).

| Command | What it does |
|---|---|
| `npm run dev` | Dev server bound to `127.0.0.1`. |
| `npm run build` | Production build. |
| `npm run start` | Serve the production build on `127.0.0.1`. |
| `npm run typecheck` | `tsc --noEmit`. |
| `npm run lint` | ESLint. |
| `npm test` | Unit tests (`node --test`). |
| `npm run test:browser` | Playwright browser tests. |

| Variable | Meaning |
|---|---|
| `GATEWAY_URL` | Where the frontend's server-side proxy reaches the gateway. |
| `PLAYWRIGHT_PORT` | Port Playwright starts the app on. |

---

## 6. HTTP endpoint index

Default base URL `http://127.0.0.1:8999`. Bodies and responses are documented in
[`docs/API_REFERENCE.md`](docs/API_REFERENCE.md). There is no authentication
(personal, loopback-only deployment).

**OpenAI-compatible**

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/chat/completions` | Chat, streaming or not. `model` is an alias or explicit `provider/model`. |
| `POST` | `/v1/completions` | Raw-prompt completions (editor autocomplete, `suffix` for fill-in-the-middle), forwarded to the provider's own `/v1/completions`. |
| `POST` | `/v1/responses` | Responses API. |
| `POST` | `/v1/embeddings` | Embeddings. |
| `GET` | `/v1/models` | Configured aliases. |

**Operations**

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness. |
| `GET` | `/providers` | Provider health, circuit state, in-flight counts. |
| `GET` | `/metrics` | Metrics snapshot. |
| `GET` | `/favicon.ico`, `/icon.png` | The application icon (embedded in the binary). |
| `POST` | `/admin/reload` | Re-read `gateway.yaml` and the env file and apply them without a restart (see below). Loopback callers only; refused for browser origins. |
| `GET` | `/admin/reload` | Reload status: watched files, reloads applied, outcome of the last attempt. |

**Reloading configuration.** The gateway re-reads its config file (and the env file) when either changes on disk (checked every 2 s, applied once the file has stopped changing) or when `POST /admin/reload` is called. Reloaded: `providers`, `models`, `routing`. Reported as "restart required" and left as running: `server`, `telemetry`, `pricing`, `caching`, `llama_server`. A file that fails to parse or validate is rejected whole - the running configuration is unchanged and the endpoint answers `422` with the reason. In-flight requests finish on the configuration they started with. Details: `docs/CONFIGURATION_GUIDE.md` section 5.

**Analytics** (read-only; `503` when no database is configured)

| Method | Path |
|---|---|
| `GET` | `/v1/analytics/summary` |
| `GET` | `/v1/analytics/requests` |
| `GET` | `/v1/analytics/requests/{request_id}` |
| `GET` | `/v1/analytics/requests/{request_id}/timeline` |
| `GET` | `/v1/analytics/providers/comparison` |
| `GET` | `/v1/analytics/fallbacks` |
| `GET` | `/v1/analytics/costs` |
| `GET` | `/v1/analytics/persistence` |

**llama-server management**: see the next section.

Quick checks:

```bash
curl http://127.0.0.1:8999/health
curl http://127.0.0.1:8999/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"general","messages":[{"role":"user","content":"hi"}]}'
```

---

## 7. llama-server management

Optional and **off by default**. It is the only feature that launches a local
process, so it also needs `server.host: 127.0.0.1` (or
`llama_server.allow_non_loopback: true`). Enable it in `config/gateway.yaml`
(see the commented `llama_server:` block there and
[`docs/CONFIGURATION_GUIDE.md`](docs/CONFIGURATION_GUIDE.md) section 4b).
Disabled means every `/llama-server/*` route answers with a `feature_disabled`
error.

Full UI-facing contract: [`docs/frontend/LLAMA_SERVER_UI.md`](docs/frontend/LLAMA_SERVER_UI.md).

### Endpoints

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/llama-server/status` | State (`stopped/starting/loading/ready/exited/failed`), loaded model, pid, uptime, probed info. |
| `POST` | `/llama-server/start` | Start a saved model. Returns `202`; poll `status`. |
| `POST` | `/llama-server/stop` | Stop. Idempotent. |
| `POST` | `/llama-server/restart` | Restart the same model with the same args. |
| `GET` | `/llama-server/logs` | Log tail, cursor-based. |
| `GET` | `/llama-server/models` | Saved models. |
| `POST` | `/llama-server/models` | Save a model (alias, gguf path, optional mmproj/args/notes). |
| `PUT` | `/llama-server/models/{id}` | Update a saved model. |
| `DELETE` | `/llama-server/models/{id}` | Delete (`409` if it is the running model). |
| `POST` | `/llama-server/models/validate` | Check paths without saving. |
| `GET` | `/llama-server/models/scan` | List `.gguf` files under the configured `scan_dirs`. |
| `GET` | `/llama-server/defaults` | Default args used when a model has none. |
| `PUT` | `/llama-server/defaults` | Set the default args. |
| `POST` | `/llama-server/args/parse` | Tokenize an args string and report errors. |

Query/body arguments:

| Endpoint | Argument | Meaning |
|---|---|---|
| `POST start` | `model_id` | Id of a saved model (required). |
| `POST start` | `args` | Optional args string overriding the model's saved args for this launch. |
| `POST start` | `replace` | `true` stops a running server first (the "switch model" flow). Otherwise `409 already_running`. |
| `GET logs` | `after` | Return lines with `seq` greater than this cursor. |
| `GET logs` | `limit` | Maximum lines to return. |

Args precedence: request override, then the model's saved args, then the default
args. Example:

```bash
curl -X POST http://127.0.0.1:8999/llama-server/models \
  -H 'Content-Type: application/json' \
  -d '{"alias":"qwen-9b","gguf_path":"S:/models/qwen3.5-9b-q4.gguf","args":"-ngl all -fa on -ctk iq4_nl -ctv iq4_nl --tools all -np 1 --ui-mcp-proxy"}'

curl -X POST http://127.0.0.1:8999/llama-server/start \
  -H 'Content-Type: application/json' -d '{"model_id":"<id from above>"}'

curl http://127.0.0.1:8999/llama-server/status
curl -X POST http://127.0.0.1:8999/llama-server/stop
```

In JSON bodies use forward slashes in Windows paths (`S:/models/...`); a bare
backslash must be escaped as `\\`.

### Saved models as chat models

With `llama_server.provider` (default `llama`) set to a configured provider, every saved model is listed in `GET /v1/models` as `llama/<alias>`, and a request for one loads it first (`auto_swap`, default on):

```bash
curl http://127.0.0.1:8999/v1/chat/completions -H 'Content-Type: application/json' \n  -d '{"model":"llama/Gemma4-E4B-AD","messages":[{"role":"user","content":"hi"}]}'
```

The request may also name a saved model's `.gguf` path. It waits for in-flight requests on the current model, replaces it, waits until ready, then runs. Names that are not saved models (like the target of a config alias) go to whatever is loaded. Details: [`docs/API_REFERENCE.md`](docs/API_REFERENCE.md) section 16.

### Argument rules for llama-server

- Args are a single string, tokenized Windows-safely: whitespace separates
  tokens, double quotes group, and backslashes are literal (so `C:\dir` survives).
- The gateway owns these flags and rejects them with `400 reserved_flag`:
  `-m`, `--model`, `-mu`, `--model-url`, `-dr`, `--docker-repo`, `-hf`, `-hfr`,
  `--hf-repo`, `-hff`, `--hf-file`, `-mm`, `--mmproj`, `-mmu`, `--mmproj-url`,
  `--host`, `--port`, `--models-dir`, `--models-preset`, `-h`, `--help`,
  `--version`, `--license`, `--completion-bash`, `--list-devices`.
- `--alias <saved alias>` is added automatically (when the model's
  `pass_alias` is on) unless you pass `-a/--alias` yourself.
- Everything else (`-ngl`, `-fa`, `-ctk`, `-c`, `-np`, `--tools`, ...) is passed
  through untouched. An unknown flag makes llama-server exit with code 1, which
  shows as state `exited` with the last log lines.

### Browser guard

Every `/llama-server/*` route rejects requests that carry an `Origin` header
(unless listed in `llama_server.allowed_origins`), `Sec-Fetch-Site: cross-site`
or `same-site`, or a non-loopback `Host` (`403 origin_forbidden` /
`host_forbidden`). Use `curl` or a server-side proxy that does not forward those
headers; a web page in your browser cannot call these routes directly.

### Stopping

Ctrl+C on the gateway stops the llama-server it started. Hard-killing the
gateway (Task Manager, `kill -9`) orphans it; it then shows as `external` and
must be stopped by hand.
