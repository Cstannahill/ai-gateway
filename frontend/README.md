# AI Gateway console

An additive React/Next.js inference console with observability and a streaming chat workspace for the existing Rust API. All application files, dependencies, build output and tests stay inside `frontend/`. No Rust code, migrations, existing docs, gateway configuration or root package files are changed.

## Run locally

Use Node 22.18+ (or a newer supported Node release) and pnpm 10.11+. This session verified Node 25.3.0 and pnpm 10.11.0. The native TypeScript test runner needs Node's built-in type stripping.

```powershell
cd S:\Code\RustProjects\AI-Gateway\frontend
pnpm install --frozen-lockfile
Copy-Item .env.example .env.local
pnpm dev
```

Open [the console](http://127.0.0.1:3000). Live mode is the default. For an explicitly labeled fixture demonstration, use **Explore demo data** or open [demo mode](http://127.0.0.1:3000/?demo=1). Demo modules load only when selected; API errors never activate demo data. Query caches are separated by mode and cleared when switching. Captured inference bodies are not persisted in browser storage. The theme and model pin/recent shortcuts are the only saved browser preferences.

Production: `pnpm build`, then `pnpm start`. Both start scripts bind only to `127.0.0.1`. No public deployment is performed or implied.

## Gateway connection

`.env.local` contains the **server-only** `GATEWAY_URL`, default `http://127.0.0.1:8999`. Use an HTTP(S) origin without a path, embedded credentials or query. No provider keys or PostgreSQL credentials belong in this app. Restart Next after changing the variable.

The browser calls same-origin `/api/gateway/*`. The Next server forwards only allowlisted GET paths and validated query parameters. It never forwards browser cookies or Authorization, follows redirects, accepts arbitrary destination URLs, or accesses the database. Upstream 404/500/503 statuses remain distinct; a network failure becomes a 502 connection error. Responses are no-store, have a 12-second timeout and an 8 MiB limit. Oversized records show an actionable error rather than freezing the page.

`localhost` from the browser is the user's machine; `localhost` inside Next is the machine/container running Next. If Next is containerized, point `GATEWAY_URL` at a reachable host or service name (for example `host.docker.internal` when supported), not the container's own loopback. This app does not alter gateway bind addresses or firewall settings. The inspected Rust router has no authentication middleware; keep both services private. Health reports gateway/provider status, not capture durability.

## What works

- Chat with up to four explicitly selected models, parallel response columns, searchable/pinnable catalog, streamed text and reasoning, a separate reasoning sidebar, safe Markdown/code/tables/details, system prompt, optional generation parameters, per-model stop, room controls, chat search, JSON/Markdown export and regeneration into a new branch. Live responses link to their request inspector.
- Overview with process counters, active provider attempts, health, bounded session activity and comparison.
- Request explorer with exact model/provider/outcome filters, offset pagination without an invented total, loaded-row sorting/columns, URL state, exact UUID navigation and CSV export.
- Accessible request drawer and full-page links; attempt/retry/switch chain, high-precision timeline, literal role-separated content, reasoning visibility, individual usage observations and lazy raw JSON/copy/export.
- Comparison with metric selection, up to six selected provider/model pairs, sortable table and scoped export.
- Recent recovery evidence with separate distinct requests/failed-attempt counts; provider/model capability inventory and local search.
- Dark/light themes, UTC/local display, keyboard controls, reduced motion, narrow layouts, pause/manual refresh and stale-data indicators.
- Local llama-server management: saved GGUF models, arguments, process status, switching, defaults, scanned paths, and live logs.

## llama-server manager

Open [llama-server](http://127.0.0.1:3000/?view=llama) or [the labelled demo](http://127.0.0.1:3000/?view=llama&demo=1). The manager is disabled by default. The disabled view shows the required `server.host: 127.0.0.1` and `llama_server` configuration, including the real `llama-server.exe` path. See [the UI contract](../docs/frontend/LLAMA_SERVER_UI.md) for the complete setup and behavior.

The browser uses a dedicated same-origin `/api/llama-server/*` proxy. It checks the route, method, origin, query, UUID, JSON body, and size before calling the gateway. The server-side outbound request uses only JSON content negotiation, so browser `Origin`, cookies, credentials, and `Sec-Fetch-*` headers never reach the guarded gateway routes. A start or switch can launch a real local process; the UI confirms disruptive switch, restart, stop, and delete actions. Demo mode uses in-memory fixtures and makes no gateway requests.

Status polls every second while starting or loading and every five seconds otherwise. Visible logs tail by cursor without browser persistence. Model paths are typed or chosen from the gateway's configured scan directories; browser file inputs cannot provide absolute paths.

No date selector is shown because current reads cannot honor a historical range. Fleet usage/spend, routing evidence, stable cursor pagination, exact choice identity and complete capture guarantees require backend additions. Current comparison and usage limitations are explained in the UI and in [API gaps](../docs/frontend/API_GAPS.md). See [metric definitions](../docs/frontend/METRICS.md) and the [feature-to-data map](../docs/frontend/IMPLEMENTATION.md).

## Chat

Open [Chat](http://127.0.0.1:3000/?view=chat) or [demo chat](http://127.0.0.1:3000/?view=chat&demo=1). Choose one to four models from `/v1/models` or explicitly enter configured aliases. No model is silently selected. One send starts one independent streamed completion per selected model through the fixed same-origin `/api/chat` route to `POST /v1/chat/completions`. Live mode can incur usage on every selected provider. Demo replies, token counts and prices are labeled fixtures and make no gateway calls. Each model receives the same user turns and only its own completed assistant answers as context; no model sees another model's response.

Conversations, drafts, response reasoning, request payloads, text attachments and generation settings stay in React memory across console navigation. Live/demo histories remain separate. Reloading clears both; JSON or Markdown export saves a copy, including prompts and reasoning. JSON export can be imported into either mode as inactive history; imported telemetry IDs and prices are not treated as live evidence. An explicit model-selection JSON file can be copied or exported without conversation content. The room menu also supports duplicate, clear, color/metric/expansion preferences, and previewed JSON character instructions as a system prompt. Text attachments (up to 256 KiB) are inserted into the draft for review before sending; images and audio are not accepted. Gateway telemetry may separately retain live requests under its existing behavior.

Changing modes stops active responses. Global Stop cancels all selected models; each response has its own Stop control. Cancellation reaches upstream connections, while actual provider termination or billing depends on the provider. Regenerate creates a new single-model branch preserving the original answer. Editing creates a new branch and clears metrics tied to the original text. Clear/delete support one-step Undo. Failed/stopped output remains visible/exportable but is excluded from later model context. Reasoning is displayed separately and is not replayed as an assistant answer.

Only text chat and one completion per selected model per send are supported. The gateway sees independent requests, with no automatic retries or tool execution. No conversation database, remote sharing, image/audio uploads or speech transcription is implied. Blank temperature/output-limit/reasoning settings omit those fields, preserving provider defaults. Parameter support remains model-specific. Output limits use the gateway's `max_completion_tokens` field. Inspector links use the gateway UUID from the streamed `chatcmpl-<uuid>` body ID, never the unrelated correlation header. A new request may need an inspector refresh while telemetry is being written.

The chat proxy permits only same-origin JSON POSTs with validated fields to the fixed completion endpoint. It forwards no browser cookies/Authorization and refuses redirects. Request bodies are limited to 1 MiB and 256 messages; streaming responses to 8 MiB, individual browser SSE records to 1 MiB, and a request to ten minutes. Stream failures retain partial text rather than reporting success. Usage displays the latest observation (never sums cumulative chunks) and warns that gateway zeros can represent unavailable counts. The current read/stream API does not expose a **per-response price**, so live chat shows an unknown cost. The whole-history cost summary cannot be divided into accurate response prices. Browser elapsed time includes client/transport time and is not backend TTFT. Markdown permits sanitized interactive `details`/`summary` blocks while removing unsafe raw HTML, remote image fetching and non-HTTP(S) navigation.

## Verification

```powershell
pnpm typecheck
pnpm lint
pnpm test
pnpm build
pnpm test:browser
```

Browser tests use installed Google Chrome (`channel: 'chrome'`) and a production server on port 3000. Change the test-only channel to `msedge` if that is your installed Chromium browser. Screenshots are generated under ignored `verification/`. The browser suite uses explicitly intercepted API-shaped fixtures and also exercises proxy rejection paths; it does not certify the Rust database or live inference providers. Unit/integration tests include a temporary loopback HTTP upstream to exercise real proxy status propagation, credential isolation, redirects and response-size limits.

The actual gateway at `127.0.0.1:8999` refused connections during this session. Live read integration is **source-contract based**, with real connection-error handling verified. No inference was submitted, database was modified, or backend process was started.

Dependencies were selected from current npm metadata and pinned in `pnpm-lock.yaml`. Next/React follow the [official installation guide](https://nextjs.org/docs/app/getting-started/installation); Tailwind uses its [official Next integration](https://tailwindcss.com/docs/installation/framework-guides/nextjs). TypeScript 5.9 and TanStack Table 8 are deliberately pinned for their verified API compatibility. Dialog primitives use Radix with a custom console presentation.
