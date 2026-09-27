# Frontend implementation map

Additions only: `frontend/` and these frontend documents. Existing Rust, configuration, migrations, tests, and documentation remain untouched.

llama-server management UI: see [LLAMA_SERVER_UI.md](LLAMA_SERVER_UI.md) (backend complete; UI to be built from that contract).

| Experience           | Verified source contract                                          | Population / boundary                                                                                   |
| -------------------- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Operational overview | `/health`; `/metrics.gateway` and `.providers`                    | Since process restart; errors and active counts are provider attempts                                   |
| Session activity     | Differences in `/metrics.gateway.total_requests`                  | Since opening the dashboard; reset/outage gaps, no historical series                                    |
| Explorer             | `/v1/analytics/requests`: model, provider, success, limit, offset | Loaded page; model matches requested OR resolved; provider matches final serving instance               |
| Investigation        | `/v1/analytics/requests/{uuid}`                                   | One request; attempts, generation records, usage observations; no final usage marker or choice identity |
| Timeline             | `/{uuid}/timeline`                                                | One request; request-relative events; no attempt-start timestamp in read contract                       |
| Comparison           | `/v1/analytics/providers/comparison`                              | All retained records; server aggregates; counts may be inflated by usage join                           |
| Reliability          | `/v1/analytics/fallbacks`                                         | At most 200 failed-attempt rows in successful requests, not 200 distinct requests                       |
| Inventory            | `/providers`, `/v1/models`                                        | Current registered instances and configured/discovered catalog; no mutation                             |
| Chat                 | `/v1/models`; `POST /v1/chat/completions` via dedicated `/api/chat` | Up to four explicit models; independent parallel text completions; streamed content/reasoning/usage; no tools or automatic retries |
| llama-server manager | `/llama-server/*` via dedicated `/api/llama-server/*` | One local process; saved GGUF models, launch settings, live status and logs; guarded same-origin mutations |
| Usage/cost           | Detail usage only; no cost read endpoint                          | Individual observations; no certified totals, pricing, or spend trends                                  |

Implementation sequence: (1) isolated Next app, transport and metric tests; (2) shell, live overview and comparison; (3) URL-driven explorer, accessible investigation drawer and timeline; (4) reliability, inventory and explicit demo fixtures; (5) build, lint, typecheck, unit and browser verification.

Design: graphite/cyan default and high-contrast light theme, persistent navigation, compact status header, scope labels beside every aggregate, tabular numeric columns, independently failing panels. No date control where no date-aware API exists. Bounded displays and explicit scoped exports. Browser data stays in memory; only theme, timezone and harmless display preferences may persist.

Evidence read: all nine requested documents, current analytics/management handlers, router middleware, writer and stream instrumentation, migrations and analytics tests. September 21 audits are historical. September 23 remediation updates supersede their implementation statuses; source reveals remaining contract limits recorded in API_GAPS.md. No authentication middleware is registered in `src/server/mod.rs`.

Chat addition: conversation state lives at the root React provider and survives console navigation, with separate live/demo sessions. Model lanes are keyed by the requested model; each receives every user turn and only its own completed assistant text. Generation settings apply to every selected model in the next send. Each stream has its own controller, status and usage, and one failure does not cancel peers. Regeneration and editing preserve the original conversation as a branch; edits/imports discard stale request IDs and prices. The dedicated POST proxy adds inference capability without broadening the observability GET allowlist. Cancellation reaches the upstream stream, partial errors remain visible, and successful completion requires a finish marker plus `[DONE]`. Source contracts: `src/types/request.rs` and `src/server/routes/chat_completions.rs::event_to_sse`. Only gateway body IDs matching `chatcmpl-<uuid>` enable live inspector links. No per-response price is exposed by current reads or stream events, so it is shown as unavailable in live mode. No backend changes were required.
