# Frontend verification — 2026-09-23

All checks apply to the additive frontend, not the Rust backend.

| Check | Result |
|---|---|
| `pnpm typecheck` | Passed, no TypeScript errors |
| `pnpm lint` | Passed, no warnings/errors |
| `pnpm test` | 21 passed; metrics, schemas, chat-session contracts, read/chat proxy, SSE and real loopback HTTP checks |
| `pnpm build` | Passed; Next production routes generated |
| `pnpm test:browser` | 20 passed against the production build in Chrome |
| Desktop screenshot review | Reviewed overview, request timeline, chat landing and streamed response at 1280 px width, plus two-model comparison at 1600 px width |
| Narrow screenshot review | Reviewed light-theme overview and chat at 390 px width; automated viewport overflow assertions passed |
| Existing-file preservation | SHA-256 hashes of all 70 pre-existing tracked files matched the initial snapshot |

Browser coverage: explicit demo/cache separation, server-filter URL state, next/previous pages without totals, drawer/permalink navigation, keyboard focus/Escape, literal untrusted content, synthetic timing, absent usage, partial generations, 404, offline, 503 with live panels still functioning, stale data after failure, head polling without row replacement, inventory search, theme/navigation, comparison selection/export, and proxy path/method rejection. Full-page request links preserve explorer query context. Tests intentionally use API-shaped fixtures; no live provider inference was issued.

Proxy integration uses a temporary loopback HTTP server and checks real fetch behavior: upstream 404/503 propagation, no-store headers, browser credential isolation, invalid JSON, refused redirects, 8 MiB response limit and invalid gateway configuration. The temporary server is closed by the test.

Chat checks additionally cover explicit model selection, settings serialization, multi-turn context, safe Markdown, reasoning, usage observations, request inspector links, history across navigation, isolated live/demo conversations, export, stop and regeneration branches. Protocol tests exercise fragmented UTF-8/CRLF, multiple events per chunk, cumulative usage replacement, malformed JSON, missing completion markers and partial errors. A real HTTP upstream proves immediate stream delivery, cancellation disconnect, no forwarded credentials, 429 propagation, origin checks, payload limits and redirect refusal. Actual Next route tests verify cross-origin rejection and same-origin schema validation without issuing inference. This caught and fixed Next's loopback URL normalization: origin comparison uses the browser-facing Host authority, not the normalized request hostname.

The expanded chat browser checks cover one prompt dispatched concurrently to two selected models, independent response lanes and cancellation, reasoning-sidebar selection, the searchable model picker, room controls, and import/export. Session contract tests verify per-model history isolation, imported request/price provenance, and Markdown export. The new two-column response view was also reviewed from a production-build screenshot. Demo metrics are labelled fixtures; live token counts come from response usage and live cost remains unavailable because the chat stream provides no per-response price.

Actual gateway probe: `127.0.0.1:8999` refused connections outside the sandbox. Therefore live telemetry retrieval, current database contents, and real-provider behavior remain unverified. The UI's field mappings and semantics are based on current source contracts; backend gaps are documented in API_GAPS.md.

Screenshots are locally generated under `frontend/verification/` (ignored build/test artifacts). Existing dirty backend changes were present before this session and were preserved. No Rust builds/tests, database writes, migration changes, root configuration edits or public deployment were performed.
