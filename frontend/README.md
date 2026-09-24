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

Open [the console](http://127.0.0.1:3000). Live mode is the default. For an explicitly labeled fixture demonstration, use **Explore demo data** or open [demo mode](http://127.0.0.1:3000/?demo=1). Demo modules load only when selected; API errors never activate demo data. Query caches are separated by mode and cleared when switching. Captured inference bodies are not persisted in browser storage. Only the theme preference is saved.

Production: `pnpm build`, then `pnpm start`. Both start scripts bind only to `127.0.0.1`. No public deployment is performed or implied.

## Gateway connection

`.env.local` contains the **server-only** `GATEWAY_URL`, default `http://127.0.0.1:8999`. Use an HTTP(S) origin without a path, embedded credentials or query. No provider keys or PostgreSQL credentials belong in this app. Restart Next after changing the variable.

The browser calls same-origin `/api/gateway/*`. The Next server forwards only allowlisted GET paths and validated query parameters. It never forwards browser cookies or Authorization, follows redirects, accepts arbitrary destination URLs, or accesses the database. Upstream 404/500/503 statuses remain distinct; a network failure becomes a 502 connection error. Responses are no-store, have a 12-second timeout and an 8 MiB limit. Oversized records show an actionable error rather than freezing the page.

`localhost` from the browser is the user's machine; `localhost` inside Next is the machine/container running Next. If Next is containerized, point `GATEWAY_URL` at a reachable host or service name (for example `host.docker.internal` when supported), not the container's own loopback. This app does not alter gateway bind addresses or firewall settings. The inspected Rust router has no authentication middleware; keep both services private. Health reports gateway/provider status, not capture durability.

## What works

- Chat with explicit model/alias selection, searchable catalog, streamed text and reasoning, safe Markdown/code/tables, system prompt, optional generation parameters, stop, conversation search/delete, JSON export and regeneration into a new branch. Live responses link to their request inspector.
- Overview with process counters, active provider attempts, health, bounded session activity and comparison.
- Request explorer with exact model/provider/outcome filters, offset pagination without an invented total, loaded-row sorting/columns, URL state, exact UUID navigation and CSV export.
- Accessible request drawer and full-page links; attempt/retry/switch chain, high-precision timeline, literal role-separated content, reasoning visibility, individual usage observations and lazy raw JSON/copy/export.
- Comparison with metric selection, up to six selected provider/model pairs, sortable table and scoped export.
- Recent recovery evidence with separate distinct requests/failed-attempt counts; provider/model capability inventory and local search.
- Dark/light themes, UTC/local display, keyboard controls, reduced motion, narrow layouts, pause/manual refresh and stale-data indicators.

No date selector is shown because current reads cannot honor a historical range. Fleet usage/spend, routing evidence, stable cursor pagination, exact choice identity and complete capture guarantees require backend additions. Current comparison and usage limitations are explained in the UI and in [API gaps](../docs/frontend/API_GAPS.md). See [metric definitions](../docs/frontend/METRICS.md) and the [feature-to-data map](../docs/frontend/IMPLEMENTATION.md).

## Chat

Open [Chat](http://127.0.0.1:3000/?view=chat) or [demo chat](http://127.0.0.1:3000/?view=chat&demo=1). Choose a model from `/v1/models` or explicitly enter a configured alias. No model is silently selected. Send uses the existing `POST /v1/chat/completions` through a dedicated same-origin Next route, `/api/chat`. It can incur normal provider usage in live mode. Demo replies are clearly labeled local fixtures and make no API calls.

Conversations, drafts, response reasoning, request payloads and generation settings stay in React memory across console navigation. Live/demo histories remain separate. Reloading clears both; JSON export explicitly saves a copy, including prompts and reasoning. Gateway telemetry may separately retain live requests under its existing behavior. Changing modes stops an active response. Stop propagates cancellation to the upstream connection; provider billing/termination is determined by the provider. Regenerate creates a new conversation containing the preceding turns and sends again, preserving the original answer. Failed/stopped assistant output stays visible and exportable but is excluded from subsequent model context. Completed assistant text and user messages form the context; reasoning is not replayed as user-facing text.

Only text chat and one completion per send are supported. No automatic request retries, tool execution, uploads, parallel model generation or conversation database is implied. Blank temperature/output-limit/reasoning settings omit those fields, preserving provider defaults. Parameter support remains model-specific. Output limits use the gateway's `max_completion_tokens` field. Inspector links use the gateway UUID from the streamed `chatcmpl-<uuid>` body ID, never the unrelated correlation header. A new request may need an inspector refresh while telemetry is being written.

The chat proxy permits only same-origin JSON POSTs with validated fields to the fixed completion endpoint. It forwards no browser cookies/Authorization and refuses redirects. Request bodies are limited to 1 MiB and 256 messages; streaming responses to 8 MiB, individual browser SSE records to 1 MiB, and a request to ten minutes. Stream failures retain partial text rather than reporting success. Usage displays the latest observation (never sums cumulative chunks) and warns that gateway zeros can represent unavailable counts. Browser elapsed time includes client/transport time and is not backend TTFT. Markdown renders without raw HTML, remote image fetching or non-HTTP(S) navigation.

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
