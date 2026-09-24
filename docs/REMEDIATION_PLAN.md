# Remediation Plan

## Implementation status (updated 2026-09-24, session 14 — per-provider circuit breaker, Phase 5)

With `docs/frontend/API_GAPS.md` fully closed (session 13) and the dashboard now owned by a separate agent, this session picked up the next well-scoped, no-schema-change piece of Phase 5: a per-provider circuit breaker, alongside the existing `max_concurrent` enforcement (Phase 5.3) as the second resource-protection mechanism in the routing pipeline. Deliberately deterministic and config-driven — no learned/AI-driven routing, consistent with the user's standing instruction that such routing must never be pulled forward.

**New per-provider state machine (`ProviderRegistry`, `src/registry/provider_registry.rs`)**: `CircuitState::{Closed, Open{opened_at}, HalfOpen}`, tracked alongside a `consecutive_failures` counter in the existing `ProviderRuntimeState`. `record_request_end` (already the single funnel point for every attempt's success/failure, used by every route handler) now drives it: a failure increments the counter and opens the circuit once it reaches the configured `circuit_breaker_threshold`; a success resets the counter and closes the circuit unconditionally (covers both the normal case and a successful half-open recovery trial). `is_circuit_open` is the query side, and — like `is_at_capacity` — is also where the one time-based transition happens: an `Open` circuit whose cooldown (`circuit_breaker_cooldown_secs`) has elapsed transitions to `HalfOpen` on the next check, rather than via a background timer (a single-user gateway may sit idle for long stretches between requests, so there's nothing else to drive it).

**Config**: two new opt-in `ProviderConfig` fields, `circuit_breaker_threshold: Option<u32>` (`None` — the default — disables the breaker entirely, matching `max_concurrent`'s shape so existing configs see no behavior change) and `circuit_breaker_cooldown_secs: u64` (default 30). Documented in `docs/CONFIGURATION_GUIDE.md`.

**Wired into all three routing short-circuit/filter points that already check `is_at_capacity`** (`src/routing/mod.rs`): Stage 1 (explicit provider), Stage 2 (explicit model ID), and Stage 3 (alias-resolved candidate filtering) — each rejects with the new `"provider_circuit_open"` reason, recorded in `RoutingDecision.rejection_reasons` exactly like every other rejection reason, so it's visible in `/v1/analytics/requests/{id}`'s `routing` section (session 10) with no additional plumbing needed. Like `max_concurrent`, this is a resource-protection constraint, not a routing preference, so explicit `provider/model` addressing does not bypass it.

**Observability**: `ProviderStatus` (already fed to `/health`'s summary, `/metrics`, `/providers`, and the periodic `ProviderSnapshot` telemetry event) gains a `circuit_state: String` field (`"closed"`/`"open"`/`"half_open"`), computed read-only from the same state (no separate bookkeeping to drift out of sync). `/providers` picks it up automatically (it serializes `ProviderStatus` directly); `/metrics` builds its own JSON per-provider and needed the field added explicitly.

Tests (`tests/routing_tests.rs`, no DB involved — this is all in-memory `ProviderRegistry` state): `test_circuit_breaker_opens_after_consecutive_failures` (below-threshold failures don't trip it; the threshold-th consecutive failure does, and it stays open on a subsequent check with a long cooldown), `test_circuit_breaker_half_open_trial_recovers_the_circuit` (zero-cooldown config makes the Open→HalfOpen transition observable on the very next check without sleeping; a successful trial closes it), `test_circuit_breaker_half_open_failure_reopens_the_circuit` (a real 1-second cooldown and a real `tokio::time::sleep`, since this transition is driven by wall-clock elapsed time and can't be faked: confirms a failed trial re-opens the circuit with a fresh cooldown rather than closing it), `test_circuit_breaker_blocks_explicit_addressing_too` (mirrors the existing `max_concurrent` explicit-addressing test), and `test_circuit_breaker_disabled_by_default` (ten consecutive failures with no threshold configured never opens it).

`cargo test --all-targets --offline` is green: **95 passed, 0 ignored, 0 warnings** across 10 test files (up from 90 — 5 new tests, all in `routing_tests.rs`, which now has 14). No migration needed — this is pure in-memory routing/registry state, no schema touched.

`docs/ROUTING_AND_POLICIES.md` (new subsection under Stage 3 describing the state machine), `docs/CONFIGURATION_GUIDE.md` (the two new fields), `docs/ARCHITECTURE.md` (Step 2 addition), and `docs/API_REFERENCE.md` (`circuit_state` added to the `/metrics` and `/providers` sample responses) all updated.

**Remaining Phase 5 scope, still deferred and not started**: OTLP export, response caching, cost/quality-aware routing policies, dataset export workflows. (The durable telemetry outbox and `max_concurrent` enforcement — Phase 5.1/5.3 — were already done in earlier sessions; this circuit breaker is a second Phase 5.3-adjacent reliability mechanism, not a new phase item.) None of these are correctness bugs — everything shipped so far works without them; they're the next value-adds if backend work continues.

## Implementation status (updated 2026-09-23, session 13 — telemetry persistence health, closing the rest of API_GAPS item 8)

**Every item in `docs/frontend/API_GAPS.md` is now closed.** Item 8 had two independent halves; the user answered both directly: build persistence health (data-capture reliability was named as the core motivation for this entire project), and explicitly decline auth (a personal, single-user deployment never exposed beyond loopback — declined by decision, not deferred pending a future call).

**New shared `TelemetryHealth` (`src/telemetry/health.rs`)**, one instance per process, held as an `Arc` by `AppState` and by every component that can observe a real pipeline outcome:
- `TelemetryWriter` updates it on every flush attempt (`record_flush_success`/`record_flush_failure`) and on fallback-log state changes (`set_fallback_pending`, called from both `write_fallback_log`'s failure path and `replay_fallback_log`'s success/partial paths).
- Every `try_send`-full call site increments `dropped_events` — previously this was a `tracing::warn!` with no counter at all, in two independent places (`AppState::emit_telemetry` and `InstrumentedStream`'s internal `emit`) that could each silently drop events with nothing but a log line. `InstrumentedStream` gained a `telemetry_health: Arc<TelemetryHealth>` field (a new required constructor parameter) so its higher-volume per-chunk events (`StreamChunk`, `UsageRecorded`, `GenerationContent`, etc.) are covered too, not just the lower-volume lifecycle events `AppState::emit_telemetry` handles directly.
- `TelemetryWriter::health()` and `TelemetryPipeline::health()` expose the handle; `main.rs` passes it into `AppState` alongside the existing `telemetry_tx`/`analytics_pool`.

**New endpoint `GET /v1/analytics/persistence`**, deliberately the one `/v1/analytics/*` endpoint that does *not* require `analytics_pool` — it reports on the write-side pipeline, independent of the read-side query pool, so a broken analytics reader can never make this endpoint falsely report bad writer health or vice versa. Returns `{mode, healthy, channel: {capacity, pending}, last_flush_at, last_flush_ok, consecutive_flush_failures, fallback_log_pending, dropped_events_total}`:
- `mode` is `"database"`/`"unavailable"`/`"disabled"` — genuinely observed at writer construction time (`Database` if the pool connected, `Unavailable` if telemetry was enabled but the DB was unreachable at startup — this writer doesn't retry the initial connection), not inferred.
- `healthy` is deliberately narrow — `true` only when `mode == "database"` *and* the last flush succeeded *and* nothing is in the fallback log — so `"unavailable"`/`"disabled"` both read as unhealthy for data-capture purposes even though neither is an error condition per se. Matches the user's stated priority: this endpoint exists to answer "is data capture actually working," not "is anything crashing."
- `channel.pending` is computed live from `mpsc::Sender::max_capacity() - capacity()`, not tracked as separate state, so it can never drift out of sync with the real channel.

Verified three ways: `tests/api_tests.rs::test_persistence_health_endpoint_works_without_analytics_pool` (no 503 despite no analytics pool), `::test_persistence_health_counts_dropped_events_and_channel_backlog` (a capacity-1 channel, three sends, asserts exactly 2 dropped and the backlog reported), and — against a real disposable Postgres — `tests/db_verification_tests.rs::writer_health_reflects_a_real_successful_flush`, which drives one genuine flush through a real `TelemetryWriter` and checks the *same* `Arc<TelemetryHealth>` handle the writer updated, confirming `last_flush_at`/`last_flush_ok` reflect what actually happened rather than a static default.

`cargo test --all-targets --offline` is green: **90 passed, 0 ignored, 0 warnings** across 10 test files (up from 87). Ran the full DB-gated subset for real against a disposable `postgres:16` container.

`docs/frontend/API_GAPS.md` item 8 marked implemented (persistence-health half) with auth explicitly recorded as declined by user decision; the top-of-file summary now states every item in the document is closed. `docs/API_REFERENCE.md` §15 and `docs/ARCHITECTURE.md`'s Step 4 updated for the new endpoint and health-tracking behavior.

**This closes the `docs/frontend/API_GAPS.md` backlog in its entirety** — the full arc from the dashboard effort's contract analysis (session 7) through nine sessions of fixes, extensions, and one write-side schema change (per-choice generation content, session 11), ending with a genuine, if narrowly-scoped, design decision made explicitly by the user rather than assumed (usage-observation cumulative-only finding, session 12; auth-declined, this session). Future backend work here should start from a fresh look at the codebase or a new user-directed priority, not from this document — there is nothing left in it to close.

## Implementation status (updated 2026-09-23, session 12 — usage observation provenance, closing API_GAPS item 2)

Item 2 asked what "finality" means for a usage observation — whether `generation_usage` needed to model deltas, cumulative running totals, and confirmed finals as distinct states. That's a data-model question this plan couldn't answer from the code alone, so it was put to the user rather than guessed at. Their answer: check what the gateway's actual supported providers do, since that's public, documented information, not something requiring a subjective design call.

**Research finding**: OpenAI, OpenRouter, llama.cpp/llama-server, vLLM, Ollama, and Gemini's OpenAI-compat layer all follow the same documented convention — a single usage object per attempt, reporting a cumulative total for the whole response, delivered once (the final SSE chunk when `stream_options.include_usage` is set, or the single non-streaming response body). None of them stream incrementally-increasing per-chunk deltas for token counts. This resolves the open modeling question: `kind` is a real column, not a hardcoded assumption, but it is always `"cumulative"` for every provider this gateway supports today — there is no live `delta`/`final` distinction to build against actual behavior, only a documented reservation for a hypothetical future provider that doesn't follow the convention.

**Implementation**: new migration `007_usage_observation_provenance.sql` adds `generation_usage.source` (`"stream_final"`/`"non_streaming"` — derived from `InstrumentedStream`'s existing `synthetic` flag, so no new signal had to be invented) and `.kind`. `TelemetryEvent::UsageRecorded` gains both fields, set at its two construction sites (`stream_instrumentation.rs` and `embeddings.rs`, which has no streaming concept and is always `"non_streaming"`). `GET /v1/analytics/requests/{id}` now returns the raw `usage` array unchanged in shape (still preserving every observation, per the proposal's "preserve raw observations" requirement) plus a new `usage_canonical` array: one independently-derived record per attempt, each carrying `observation_count` and a `quality` flag computed in the handler (not the DB) — `"single_observation"` (the expected case per the research above), `"duplicate_observations"` (more than one *identical* observation — e.g. a redelivered telemetry event, harmless), or `"conflicting_observations"` (more than one observation with *different* values — a genuine anomaly, surfaced explicitly rather than silently averaged, summed, or resolved without comment). The existing `DISTINCT ON ... ORDER BY id DESC` canonical-row selection in the aggregate endpoints (`providers/comparison`, `summary` — fixed for a related multiplication bug two sessions ago) is unchanged; it's now known to be the *correct* model given the cumulative-only finding, not merely a convenient heuristic.

Test: `tests/analytics_tests.rs::analytics_usage_canonical_flags_single_duplicate_and_conflicting_observations` — three attempts (normal, redelivered-duplicate, genuinely-conflicting) against a real Postgres, asserting all three quality states and that the conflicting case's canonical value is the most recent observation, not a derived average or sum.

`cargo test --all-targets --offline` is green: **87 passed, 0 ignored, 0 warnings** across 10 test files (up from 86). Ran the full DB-gated subset for real against a disposable `postgres:16` container; migration `007` applied cleanly alongside the existing six.

`docs/frontend/API_GAPS.md` item 2 marked implemented (with the research finding recorded so a future reader doesn't have to re-derive it), and the top-of-file summary now lists items 1–7 as fully closed, with only item 8 remaining. `docs/API_REFERENCE.md` §10 and `docs/TELEMETRY_AND_DATABASE.md` (new "Usage observation semantics" subsection) updated for the new columns and response shape.

**Only item 8 (persistence health + auth) remains open in `docs/frontend/API_GAPS.md`.** Per the user's explicit direction: this is a personal, single-user deployment never exposed beyond loopback, so **auth is out of scope** — not deferred pending a future decision, but decided against for this project. Only the persistence-health half is planned: a read contract reporting whether the telemetry writer is actually flushing, how far behind it is, and pending/dropped counts, distinct from `/health`'s provider-only view. The user specifically named telemetry/data-capture reliability as the core motivation for this whole project, so this is the natural next piece of work.

## Implementation status (updated 2026-09-23, session 11 — per-choice generation content, closing the rest of API_GAPS item 5)

At the user's explicit direction, this session does the write-side work session 10 deliberately deferred: separating `n > 1`'s choices at the point content is captured, not just at the point it's read.

**`InstrumentedStream` now accumulates content, reasoning, and tool calls per choice index instead of merging every choice into one accumulator.** `ProviderEvent::ContentDelta` and `ToolCallDelta` already carried an `index` field identifying which choice a delta belongs to (added when `n > 1` support first landed — Phase 2.3) — it was captured on the wire but discarded before persistence, since `accumulated_content`/`tool_calls_map` were flat (`String`/`BTreeMap<tool_call_index, _>` with no choice dimension). They're now `BTreeMap<choice_index, String>` and `BTreeMap<choice_index, BTreeMap<tool_call_index, _>>` respectively. Reasoning stays a single un-indexed accumulator, attributed only to choice 0 — `ReasoningDelta` carries no `index` field in the provider-event model, and reasoning-capable models are not commonly combined with `n > 1` in practice (the same scoping decision recorded when `n > 1` support first landed).

**`TelemetryEvent::GenerationContent` gains `choice_index: u32` and `status: String` (`"complete"`/`"partial"`), and is now emitted once per choice** on both the clean-completion path (`ProviderEvent::Completed`) and the mid-stream-error partial-content-preservation path (`ProviderEvent::Error`, Audit A04) — via a new `emit_generation_content_per_choice` helper that fans out over every choice index present in either accumulator, rather than one call site per branch duplicating the same logic (as the old single-choice code did).

**New migration `006_generation_choice_index.sql`** adds `generations.choice_index` (existing rows default to `0`, the only value that ever existed before this session) and `generations.status`. `generations.id` — already a `BIGSERIAL` primary key, previously never exposed — is now returned by the analytics API as the "stable ID" `docs/frontend/API_GAPS.md` item 5 asked for. `GET /v1/analytics/requests/{id}`'s `generations[]` now includes `id`, `choice_index`, and `status`; the reasoning-visibility computation in `writer.rs` skips its `generation_usage` lookup entirely for non-zero choice indices (reasoning is never populated there by construction, so the query would always find nothing).

Verified two ways: `tests/audit_contract_tests.rs::telemetry_emits_separate_generation_content_per_choice_for_n_greater_than_one` and `::telemetry_preserves_partial_content_per_choice_on_mid_stream_error` drive an `n > 1` request through the *real* handler/provider/`InstrumentedStream` pipeline (a new ProbeProvider mode 4 emits two choices' content then a mid-stream error) and inspect the drained telemetry channel directly — these test the write-side logic itself, not just the schema. `tests/analytics_tests.rs::analytics_request_detail_exposes_stable_id_and_choice_index_per_generation` verifies the read side against a real Postgres: two choices come back as two rows with distinct `id`s and correct `choice_index`/`reasoning_content` attribution (reasoning only on choice 0).

`cargo test --all-targets --offline` is green: **86 passed, 0 ignored, 0 warnings** across 10 test files (up from 83 — three new tests). Ran the full DB-gated subset for real against a disposable `postgres:16` container; migration `006` applied cleanly alongside the existing five.

`docs/frontend/API_GAPS.md` item 5 updated to "fully implemented," and its top-of-file summary now lists only items 2 and 8 as open. `docs/API_REFERENCE.md` §10 and `docs/TELEMETRY_AND_DATABASE.md`'s schema table updated for the new columns and response shape.

**Everything in `docs/frontend/API_GAPS.md` is now closed except:**
- **Item 2** (usage observation identity/finality) — a genuine data-model decision about what "finality" means for a value this schema currently just re-inserts with no marker distinguishing a delta from a running total from a confirmed final count. Needs the user's input on the model before implementing, not an engineering judgment call to make unilaterally.
- **Item 8** (persistence health + auth) — the persistence-health half (a `/v1/analytics/health`-style contract distinct from `/health`'s provider-only view) is bounded, ordinary work; the auth half (a real access-control boundary for any non-loopback deployment — no auth middleware exists at all today) is a security-sensitive design decision that needs the user's input on approach (what auth model, how secrets are managed) before implementing, not something to improvise unprompted.

## Implementation status (updated 2026-09-23, session 10 — request-detail provenance + timeline pagination, closing the non-choice-ID pieces of API_GAPS item 5)

Continues through `docs/frontend/API_GAPS.md`'s backlog, at the user's explicit direction to do item 5's non-choice-ID pieces specifically (the choice-ID piece needs a write-side change to `GenerationContent`'s per-attempt content model, not just a new query, and stays out of scope here).

**Attempt spans.** `provider_attempts.timestamp` was always the attempt's start time but was never named that in any API response, and there was no column at all for when an attempt finished — only `duration_us`. New migration `005_attempt_finished_at.sql` adds `finished_at`, set by `writer.rs`'s `ProviderAttemptCompleted` handler from that event's own `timestamp` field (previously received but never persisted). Request detail's `attempts[]` now exposes `started_at`/`finished_at` plus a fixed `duration_definition` string stating explicitly what these measure (dispatch-to-terminal-event, not TCP-connection or full end-to-end latency — the exact caveat `docs/frontend/API_GAPS.md` item 5 asked not to leave implicit).

**Routing/errors/costs sections.** `GET /v1/analytics/requests/{id}` previously returned only `request`/`attempts`/`generations`/`usage`. Added `routing` (from `routing_decisions` — candidates considered/rejected, rejection reasons, selected provider/model, policy applied), `errors` (from `errors` — structured per-attempt failure records), and `costs` (from `cost_records` — the per-request view of the cost data session 7's `/v1/analytics/costs` endpoint only ever summarized in aggregate). All three tables already had writers from earlier phases; none had a reader at the per-request level until now.

**Cursor-paginated timeline.** `GET /v1/analytics/requests/{id}/timeline` gains `after`/`limit` params (default 1000, capped 5000) and returns `{events, next_cursor, complete}`, cursoring on `(attempt_number, sequence)` — the timeline's own natural ordering key. Simpler than the request-list's keyset cursor (session 9): stream events are append-only per attempt with no equivalent concurrent-insert race, so the cursor here exists purely to bound response size for a long stream, not to fix a correctness bug.

Tests (both against a real disposable Postgres): `tests/analytics_tests.rs::analytics_request_detail_includes_attempt_spans_routing_errors_and_costs` (a two-attempt request — one fails with a routing rejection and an `errors` row, one succeeds with a priced `cost_records` row — asserts all three new sections and that `finished_at > started_at`) and `::analytics_timeline_cursor_pagination_returns_each_event_exactly_once` (5 seeded stream events, paginated at `limit=2` across 3 pages, asserts the exact ordered sequence `[1,2,3,4,5]` with `complete` only true on the final page).

`cargo test --all-targets --offline` is green: **83 passed, 0 ignored, 0 warnings** across 10 test files (up from 81). Ran the full DB-gated subset for real against a disposable `postgres:16` container — migration `005` applied cleanly alongside the existing 4.

`docs/frontend/API_GAPS.md` updated with item 5's status note (explicit about the choice-ID exclusion and why) and the top-of-file summary. `docs/API_REFERENCE.md` §10/§11 and `docs/TELEMETRY_AND_DATABASE.md`'s schema table updated for the new column and response shapes.

**Still open from `docs/frontend/API_GAPS.md`:** item 2 (usage observation identity/finality — a data-model decision), item 5's choice-ID piece (needs `GenerationContent`/write-path changes), item 8 (persistence health + auth — auth is a security-sensitive design decision, not something to improvise unprompted). Everything else in the document is closed.

## Implementation status (updated 2026-09-23, session 9 — keyset pagination for `/v1/analytics/requests`, closing API_GAPS item 6)

Continues through `docs/frontend/API_GAPS.md`'s backlog. Item 6 was next: a real correctness bug (offset pagination can duplicate/omit rows under concurrent inserts — the same class as session 7's two bugs, just in the one endpoint that hadn't been touched yet), plus filter gaps, with no data-model decision required (unlike item 2) and no write-side change required (unlike item 5).

**`GET /v1/analytics/requests` rewritten from offset to keyset pagination — a breaking change to this endpoint's contract.** Previously `ORDER BY received_at DESC LIMIT $n OFFSET $m`: an offset counts row *positions*, which shift whenever a row is inserted ahead of the current page, so polling page 2 after a new request arrived could reshow part of page 1 or skip a row entirely. Now `ORDER BY (received_at DESC, request_id DESC)` with an opaque `cursor` param encoding the last row's own `(received_at, request_id)` — a stable position in the data, not a shifting count. Response shape changed from `{requests, limit, offset}` to `{requests, next_cursor, as_of, limit}`. `offset` is gone entirely; there is no dual-mode compatibility shim, since this API has never been treated as a stable/versioned contract (every prior session's fix has changed some endpoint's shape) and the frontend's own workaround for the old bug — polling a separate first-page query — is superseded outright by `as_of`.

**New filters, closing the rest of item 6's ask:** `from`/`to` (UTC bounds on `received_at`), `application`, `task`, `tag` (exact matches, the last via `EXISTS` against `request_tags`), and `attempted_provider` — deliberately kept separate from the existing `provider` filter, which matches only the *final serving* provider. A request that failed on provider A and succeeded on provider B now matches `attempted_provider=A` but not `provider=A`, and matches both `attempted_provider=B` and `provider=B`.

**One proposed piece deliberately not built:** the proposal's "an allowlist of sort fields... invalid sorts rejected" implies caller-selectable sorting. Ordering is fixed at `(received_at DESC, request_id DESC)` with no sort parameter at all — nothing to validate since there's nothing to choose. The UI's actual stated need was stable traversal, which a fixed order already provides; configurable sorting wasn't asked for by any concrete use case, so it wasn't spec'd here rather than guessed at.

Tests: `tests/analytics_tests.rs::analytics_requests_keyset_pagination_is_stable_under_concurrent_inserts` (seeds two requests with an exact tied `received_at` plus two older ones, fetches page 1 at `limit=2`, inserts a new request newer than everything already fetched — simulating the race the old bug hit — then fetches page 2 via cursor, and asserts the two pages together cover exactly the four original requests with no duplication or omission, and that the newly-inserted row never leaks into the continuation) and `::analytics_requests_supports_attempted_provider_application_task_and_tag_filters` (a two-attempt request that fails on one provider and succeeds on another, verifying `attempted_provider` and `provider` disagree correctly for the failed one).

`cargo test --all-targets --offline` is green: **81 passed, 0 ignored, 0 warnings** across 10 test files (up from 79). Ran the full DB-gated subset for real against a disposable `postgres:16` container. `docs/frontend/API_GAPS.md` updated with item 6's status note (flagging the breaking change explicitly) and its top-of-file summary line.

**Still open from `docs/frontend/API_GAPS.md`:** item 2 (usage observation identity/finality — a data-model decision), item 5 (request-detail provenance extension + cursor-paginated timeline; the choice-ID piece needs a write-side change, not just a new query), item 8 (persistence health + auth — auth in particular is a security-sensitive design decision, not something to improvise unprompted). None are correctness bugs in what exists.

## Implementation status (updated 2026-09-23, session 8 — `GET /v1/analytics/summary`, closing API_GAPS item 1)

Continues session 7's work through `docs/frontend/API_GAPS.md`'s backlog. Item 1 ("historical summary and series") was the largest of the four originally deferred items but needed no new data-model decision (unlike item 2) and no write-side change (unlike item 5's choice-ID piece) — pure new read-endpoint work, so it was the natural next pick.

**New endpoint — `GET /v1/analytics/summary?from=&to=&bucket=minute|hour|day&model=&provider=`.** Returns `{scope, requests: {total, successful, failed, pending}, attempts: {total, failed}, usage: {prompt, completion, cached, reasoning, known_requests, eligible_requests}, series: [...]}` over a half-open `[from, to)` window bucketed at stable UTC unit boundaries (`date_trunc` on `received_at AT TIME ZONE 'UTC'`, not aligned to `from`). Matches the proposal's semantics exactly:
- `pending` = `completed_at IS NULL` (received, no `RequestCompleted` yet — in flight or lost), kept distinct from `failed` (`success = false`, a genuine terminal outcome).
- `usage` fields are `null`, not `0`, when no usage was ever recorded in scope.
- Usage is de-duplicated via the same canonical-row-per-attempt pattern as `providers/comparison` (session 7), so an attempt with repeated cumulative `UsageRecorded` updates is never summed twice.
- `provider` filters on `serving_provider` explicitly (`scope.provider_semantics` states this in the response itself, not just in docs), never an attempted-but-failed provider.
- A `SUMMARY_MAX_BUCKETS` (500) cap rejects an oversized range/bucket combination with `400 range_too_large` rather than running an unbounded scan.

**Bug found and fixed during verification, same class as session 7's two:** Postgres's `SUM()` over a `bigint` column returns `NUMERIC`, not `BIGINT` — the usage-aggregation queries (both the whole-window totals and the per-bucket series) bound their sums into `Option<i64>` fields and failed every single call with a `sqlx` type-mismatch decode error until each `SUM(...)` was cast `::bigint` explicitly. Caught immediately by the real-database test, not by `cargo check` (this is a runtime type mismatch sqlx can only catch when the query actually executes against a live connection) — reinforcing why `docs/db_verification_tests.rs`/`analytics_tests.rs`'s opt-in-real-Postgres pattern exists rather than trusting compilation alone for anything touching `SUM`/`AVG`/`ROUND` over integer columns.

Also fixed in passing: an RFC3339 timestamp's default `+00:00` UTC suffix contains a literal `+`, which standard query-string form-encoding decodes as a space — corrupting the timestamp for any hand-constructed URL. Documented in the endpoint's own doc comment and `docs/API_REFERENCE.md` §8 (use the `Z` suffix form, or percent-encode).

Test: `tests/analytics_tests.rs::analytics_summary_covers_boundaries_retries_pending_and_usage_dedup` — seeds exactly item 1's acceptance criteria in one scenario (a request at the `from` boundary, one at the `to` boundary that must be excluded, a 2-attempt retry chain, a pending request with no completion event at all, a failed request with no usage ever recorded, and a request with two cumulative usage updates on one attempt) and asserts both the whole-window totals and the per-bucket series are correct. Plus `::analytics_summary_rejects_invalid_bucket_and_oversized_range` for the two validation paths.

`docs/frontend/API_GAPS.md` updated with a status note on item 1 (and, since it hadn't been done yet, retroactive status notes on items 3/4/7 from session 7 — each documents any deviation from its original proposal, e.g. `costs` has no date-range/series scoping yet, `comparison`'s new fields are flat rather than nested).

`cargo test --all-targets --offline` is green: **79 passed, 0 ignored, 0 warnings** across 10 test files (up from 77). Ran the full DB-gated subset for real against a disposable `postgres:16` container.

**Still open from `docs/frontend/API_GAPS.md`** (unchanged from session 7's assessment — each needs its own design pass): item 2 (usage observation identity/finality — a data-model decision), item 5 (request-detail provenance extension + cursor pagination; the choice-ID piece needs a write-side change, not just a new query), item 6 (keyset pagination + real filters for the request list), item 8 (persistence health + auth). None are correctness bugs in what exists — the frontend already degrades gracefully around their absence per `docs/frontend/METRICS.md`.

## Implementation status (updated 2026-09-23, session 7 — analytics API gaps from `docs/frontend/API_GAPS.md`)

The separate dashboard effort (Phase 4.2, owned by another agent) landed and produced `docs/frontend/API_GAPS.md` and `docs/frontend/METRICS.md` — a rigorous, frontend-driven contract analysis of the `/v1/analytics/*` API from session 5, written by actually trying to build a UI against it. It surfaced two genuine correctness bugs (not just missing features) in code from this project, plus a backlog of well-specified proposed API extensions. This session fixed the bugs and closed the two most bounded, highest-value gaps; the larger ones are tracked below rather than rushed.

**Bug fixed — provider comparison inflated attempt counts (API_GAPS item 3).** `GET /v1/analytics/providers/comparison` joined `provider_attempts` directly against `generation_usage`, but that table can carry more than one row per `(request_id, attempt_number)` — every `UsageRecorded` event inserts a new row rather than upserting one. The join duplicated every attempt row by its usage-row count, inflating `total_attempts`/`successful_attempts` and skewing every average's weighting. Fixed with a `LEFT JOIN LATERAL ... ORDER BY id DESC LIMIT 1` that selects exactly one canonical (most-recently-inserted) usage row per attempt before joining, so the query can never multiply a row. Also added `duration_sample_count`/`throughput_sample_count`/`ttft_sample_count`, `p50_ttft_ms`/`p95_ttft_ms`, and `synthetic_attempts` to the response, since the query was already being touched and these were explicitly requested (item 3's acceptance criteria: "counts reflect each metric's actual population... percentiles come from eligible raw observations"). Test: `tests/analytics_tests.rs::analytics_provider_comparison_does_not_multiply_attempts_with_multiple_usage_rows` (seeds one attempt with two usage rows, asserts `total_attempts == 1`).

**Bug fixed — fallback/recovery endpoint capped joined rows, not requests (API_GAPS item 7).** `GET /v1/analytics/fallbacks` applied `LIMIT 200` *after* joining `provider_attempts`, so a handful of requests with many failed attempts could crowd out others from the sample, despite the endpoint documenting itself as returning "the 200 most recent such requests" — a real scoping bug. Rewritten as two queries: first select up to 200 *distinct requests* (successful overall, with at least one non-success attempt), then fetch each one's *full* attempt chain (not just the failures) so the whole recovery sequence — same-provider retry, model switch, provider switch — is reconstructable. Response shape changed from a flat list of `{request_id, failed_provider, ...}` rows to `{fallbacks: [{request_id, succeeded_provider, attempts: [...]}], sample_scope, truncated}`; `truncated` is honest about whether older recovered requests exist beyond the sample. Test: `tests/analytics_tests.rs::analytics_fallback_analysis_reflects_a_failed_then_succeeded_request` (updated) verifies the full two-attempt chain in order.

**New endpoint — `GET /v1/analytics/costs` (API_GAPS item 4).** `cost_records` has had a writer since Phase 3.5 but no reader at all — every cost ever computed was write-only, unreachable from any API. New handler returns `{currency, estimated_token_charge, billed_cost: null, priced_attempts, unpriced_attempts, eligible_attempts, pricing_source, breakdowns}`, with an honest priced/unpriced/eligible distinction (a missing price is not a zero cost; an explicitly configured zero price is priced) and `billed_cost` always `null` (no concept of an actual provider invoice exists — `cost_records.actual_cost` is schema-only, nothing populates it, and this endpoint doesn't pretend otherwise). Optional `provider`/`model` query filters. Test: `tests/analytics_tests.rs::analytics_cost_summary_distinguishes_priced_from_unpriced_attempts`.

`cargo test --all-targets --offline` is green: **77 passed, 0 ignored, 0 warnings** across 10 test files (up from 75). Ran the full DB-gated subset for real against a disposable `postgres:16` container, including all new/changed analytics tests.

**Deliberately not attempted this session** (larger items from `docs/frontend/API_GAPS.md`, each needing its own design pass rather than a rushed version):
- **Item 1 (historical summary/series)** — a new `GET /v1/analytics/summary` with date-range bucketing (`minute|hour|day`), half-open `[from,to)` UTC intervals, and a bounded max-bucket-count guard. Genuinely new machinery (bucket-boundary generation, gap-filling for empty buckets) that item 4's cost series could eventually share — worth designing once for both rather than building two divergent bucketing implementations.
- **Item 2 (usage observation identity/finality)** — proposing that usage rows carry `{id, observed_at, kind: delta|cumulative|final, source}` and that responses include an independently derived canonical-final record per attempt. This is a **data model decision**, not just a query change: it asks what "finality" means for a value this codebase currently just re-inserts on every update with no marker distinguishing a delta from a running total from a confirmed final count. Session 7's `LATERAL ... LIMIT 1` fix in the comparison query is a reasonable interim proxy (most-recent-by-id-as-canonical) but is explicitly not this — it doesn't record *why* a row is canonical, just picks one.
- **Item 5 (request-detail provenance extension)** — adding `started_at`/`finished_at` to attempts, stable generation/choice IDs, separate `routing`/`errors`/`costs` sections, and cursor-paginated timeline (`after`/`limit`/`next_cursor`). The choice-ID piece specifically can't be added as a read-only projection: `GenerationContent` currently combines all choices' content at attempt level (a session-4 scoping decision — see Phase 2.3's `n > 1` notes), so recovering per-choice identity needs a write-side change to what's captured, not just a new read query.
- **Item 6 (keyset pagination + real filters for the request list)** — replacing offset pagination with `(received_at DESC, request_id DESC)` keyset cursors, adding UTC from/to and application/task/tag filters, and a separate attempted-provider-vs-serving-provider filter distinction.
- **Item 8 (persistence health + auth)** — a `/v1/analytics/health`-style contract exposing writer state/last-flush/pending-dropped-counts distinct from `/health`'s provider-only view, and a real access-control boundary for any non-loopback deployment (no auth middleware exists at all today — `docs/frontend/API_GAPS.md` is explicit that optional-Authorization examples elsewhere are not evidence of authentication).

None of these are correctness bugs in what exists today (unlike items 3 and 7, which were) — they're well-specified feature gaps the frontend already degrades gracefully around per `docs/frontend/METRICS.md` ("—" means unavailable, not zero). Picking these up should start from `docs/frontend/API_GAPS.md` directly rather than re-deriving requirements from scratch.

## Implementation status (updated 2026-09-23, session 6 — Phase 5.1 durable outbox + Phase 5.3 max_concurrent)

Per the user's direction: another agent owns Phase 4.2/4.3 (the dashboard and Python layer) separately, so this session continues the backend/logic track — Phase 5 items that don't depend on a dashboard existing, excluding learned/AI-driven routing (permanently out of scope).

**Phase 5.1 (durable local journal/outbox) upgraded from "fallback file, no replay" to "fallback file with automatic replay-on-recovery"**: `src/telemetry/writer.rs` gains `TelemetryWriter::replay_fallback_log`, which reads the JSONL fallback log, parses each line as a `TelemetryEvent` (skipping and warning on any malformed line rather than blocking the rest — a partial write from a crash mid-append shouldn't poison the whole file), and flushes the parsed events through the normal transactional path in `batch_size`-sized chunks. It's called once on `TelemetryWriter` construction (so a process restart after an outage immediately drains whatever was stranded) and again on every periodic flush tick (so a long-running process recovers automatically once Postgres comes back, no restart required). On a partial failure mid-replay, the file is rewritten with just the events that didn't make it, so a later attempt resumes rather than reprocessing already-persisted events.

This is explicitly **not** the full spec from Phase 5.1's original description ("a write-ahead local log with checkpointing... survives extended Postgres outages without any risk of loss") — events are still only journaled *after* an in-memory batch has already failed its DB attempts, not before every write, so a hard process crash between receiving an event and a failed flush can still lose it. What's closed is the specific gap the original minimal stopgap (Phase 0.3) left open: nothing previously ever read the fallback file back, so a real outage degraded from "lost" to "requires an operator to notice the file and write a replay tool" rather than to "delayed." Now it's actually "delayed" — the gateway recovers on its own.

Verified against a real disposable Postgres (`tests/db_verification_tests.rs::fallback_log_is_replayed_and_cleared_on_writer_startup`): manually wrote a JSONL file containing two stranded requests' worth of events plus one malformed line, constructed a fresh `TelemetryWriter` pointed at it with a live database, and confirmed both requests landed in `inference_requests`, the malformed line was skipped without blocking the rest, and the fallback file was removed. `docs/ARCHITECTURE.md`'s Step 4 and the module-level doc comments in `writer.rs` now describe this accurately instead of only describing the one-way fallback write.

**Phase 5.3 (partial: `max_concurrent` enforcement)**: this config field was parsed into `ProviderConfig` and stored but never read anywhere — accepted, then silently ignored, the exact class of defect the original audits flagged for other fields (`stop`-as-string, `developer` role, etc. in Phase 2.3). `ProviderRegistry::is_at_capacity` checks a provider's current in-flight count (already tracked via `record_request_start`/`record_request_end` for `/metrics`) against its configured `max_concurrent`. Wired into `Router::route` at all three points that select a candidate: Stage 3's availability filter (alias resolution — a saturated candidate is rejected with reason `"provider_at_capacity"`, distinct from `"provider_unavailable"`, and the router falls through to the next candidate), and both of Stage 1/2's explicit-addressing short-circuits (same rationale as the `enabled` kill switch added in an earlier session: a resource-protection constraint must not be bypassable by explicit addressing, since that would make the limit trivially avoidable). Tests: `tests/routing_tests.rs::test_max_concurrent_filtering` (alias routing falls through to a non-saturated candidate) and `::test_max_concurrent_blocks_explicit_addressing_too`. Documented in `docs/CONFIGURATION_GUIDE.md` and `docs/ROUTING_AND_POLICIES.md`'s Stage 3 section (which also had a stale rejection-reason string fixed while there).

Circuit breakers, health scoring beyond binary healthy/unhealthy, and rate limiting (the rest of Phase 5.3) are not attempted — `max_concurrent` enforcement was the one piece of that item that was pure dead code with an obvious fix; the others are new subsystems needing their own design pass (what should trip a circuit breaker and for how long; what a rate-limit policy actually looks like per-provider vs. per-client).

`cargo test --all-targets --offline` is green: **75 passed, 0 ignored, 0 warnings** across 10 test files (up from 72 at the start of this session — three new tests: the outbox replay test and two `max_concurrent` tests). Also ran the full DB-gated subset for real against a disposable `postgres:16` container.

**Still open, deliberately not attempted this session** (large, separable pieces of work — flagging rather than starting them speculatively):
- Full write-ahead journaling with checkpointing (the complete Phase 5.1 spec, vs. this session's bounded upgrade).
- OTLP export (Phase 5.2) — an additive export layer once this session's fixes make the underlying data trustworthy enough to be worth exporting; needs a new dependency (`opentelemetry`/`opentelemetry-otlp` crates) and its own design pass for what to map from this schema.
- Circuit breakers, provider health scoring beyond binary healthy/unhealthy, rate limiting (remainder of Phase 5.3, beyond the `max_concurrent` piece closed above).
- Prompt/semantic caching (Phase 5.4).
- Real selection logic for `cheapest`/`highest_quality`/`balanced`/`local_first` routing policies (Phase 5.5) — the dispatch mechanism already rejects these loudly rather than silently substituting ordered routing (Phase 1.5); only the actual policy logic remains unimplemented, which needs a pricing/quality data model decision first.
- Dataset candidate/annotation/preference-pair workflows (Phase 5.6) — downstream of the dashboard (Phase 4.2, owned by a separate agent) per the plan's own sequencing.
- Learned/AI-driven routing (Phase 5.7) — **permanently out of scope**, per the user's explicit original instruction. Never attempt this regardless of any other scope change.

## Implementation status (updated 2026-09-23, session 5 — Phase 3 closed + Phase 4 item 1)

Per explicit user scope decision this session: finish Phase 3, build Phase 4 item 1 (the Rust analytics query API) only — skip the Next.js dashboard, the Python companion, and all of Phase 5 (which the plan itself says not to pull forward; learned/AI-driven routing stays permanently out of scope regardless).

**Phase 3.1 (timing model) closed the remainder:**
- `src/telemetry/stream_instrumentation.rs::InstrumentedStream` now computes attempt `duration_us` from a monotonic `std::time::Instant` captured at construction, not `DateTime<Utc>` subtraction — safe across an NTP adjustment mid-request.
- Added a `synthetic: bool` field to `TelemetryEvent::StreamChunk`/`FirstProviderByte`/`FirstGenerationChunk`, threaded through a new `InstrumentedStream::new` parameter, set by both `chat_completions.rs` and `responses.rs` to `!request.stream` (equivalently `!stream`) — true whenever the adapter took its non-streaming branch and replayed a single HTTP response as a one-shot event sequence, whose events all land within microseconds of each other and would otherwise silently pollute TTFT/TPS/inter-chunk-latency averages with fabricated near-zero timing (the exact defect Audit A06 named). New migration `004_synthetic_timing_flag.sql` adds `stream_events.synthetic` and a sticky (OR'd) `provider_attempts.synthetic_timing`. `docs/TELEMETRY_AND_DATABASE.md`'s analytical queries now filter on it, and its formulas section leads with an explicit warning to do so.
- Still not done (unchanged from prior sessions, and reasonable to leave unfixed per the plan's own framing): `Connected` still measures response-headers-received, not TCP/TLS establishment — `reqwest` has no connection-hook without a custom connector.

**Phase 3.2/3.4/3.5 — verified against a real database, not just code review**, closing the last "verified by code review only" caveat from session 2. Spun up a disposable `postgres:16` container, ran the actual migration set (which surfaced and fixed a real bug — see below), and added `tests/db_verification_tests.rs` (3 tests, opt-in via `AUDIT_DATABASE_URL`, graceful no-op otherwise): reasoning-visibility ordering both directions (`GenerationContent` before/after `UsageRecorded`) plus `exposed`/`none` cases, `model_snapshots`/`provider_snapshots` persistence, and `cost_records` computation with configured pricing. All three passed on first real-database run once the bug below was fixed.
- **Bug found and fixed by this verification**: nothing in this codebase tracks new files added to `migrations/` for `sqlx::migrate!`'s cache-invalidation, so a `cargo check`/`cargo test` run that doesn't touch `writer.rs` itself can silently keep using a stale embedded migration set — migration `004` didn't apply on the first real run against a fresh database, and every query referencing its new columns then failed, silently degrading the entire batch to the JSONL fallback log (correct *behavior* — no data was lost — but with a genuinely stale migration set, invisible without a real database). No code fix was needed (touching `writer.rs` forces re-embedding), but this is worth knowing: **after adding a migration file, touch `src/telemetry/writer.rs` (or otherwise force a rebuild) before trusting a test run against a real database.**
- Also fixed a **second real bug found by this verification**: the new `/v1/analytics/providers/comparison` query's `ROUND(...)` aggregates returned Postgres `NUMERIC`, which `sqlx` cannot decode into `f64` — every such expression now casts to `::float8` explicitly. This would have been an unconditional 500 on that endpoint in production; no non-database test could have caught it.
- The previously-`#[ignore]`d Audit A02 probe (`database_batch_must_preserve_chunks_for_new_requests`) now un-ignored and converted to the same opt-in-graceful-skip pattern (checks `AUDIT_DATABASE_URL`, `return`s if unset instead of panicking) — it passed cleanly against the real database.

**Phase 4 item 1 (Rust analytics query API) implemented**: new `src/server/routes/analytics.rs`, mounted at `/v1/analytics/*` via a new `AppState.analytics_pool: Option<sqlx::PgPool>` (a separate read-side pool from `TelemetryWriter`'s own write-side one, built in `main.rs` from the same `DATABASE_URL`). Five endpoints: `GET /v1/analytics/requests` (list/filter/paginate), `GET /v1/analytics/requests/{id}` (full detail — parent row, attempts, generations, usage), `GET /v1/analytics/requests/{id}/timeline` (chunk-level `stream_events`, `synthetic` exposed not filtered), `GET /v1/analytics/providers/comparison` (per-provider/model success rate, avg latency, avg TPS, avg TTFT — all timing averages excluding `synthetic_timing`), `GET /v1/analytics/fallbacks` (fallback frequency/root cause, capped at 200 most recent). Every endpoint returns `503 analytics_unavailable` instead of panicking or silently returning empty data when no database is configured/reachable. Tested end-to-end against a real database in `tests/analytics_tests.rs` (4 tests: 503-without-DB, requests list+detail+404, timeline+provider-comparison, fallback-analysis), plus documented in `docs/API_REFERENCE.md` (§8–12). No new tables, no mutation — purely a query surface over the existing schema, scoped exactly to what the plan called for ("a thin layer... request/attempt/timeline query endpoints, comparison queries") and explicitly not the Next.js dashboard or Python layer (Phase 4 items 2–3, out of scope this session).

`cargo test --all-targets --offline` is green: **72 passed, 0 ignored, 0 warnings** across 10 test files (up from 64/1 — two new files, `db_verification_tests.rs` and `analytics_tests.rs`, plus the un-ignored A02 probe). Confirmed with a forced rebuild that zero warnings survive. Also ran the DB-dependent subset for real against a disposable `postgres:16` container (`docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:16`) — all 7 DB-gated tests pass, not just compile.

**Remaining after this session**: Phase 4 items 2 (Next.js dashboard) and 3 (Python analysis companion), and all of Phase 5, per explicit user decision to defer them — not a gap discovered by this session's work, a scope boundary chosen deliberately.

## Implementation status (updated 2026-09-22, session 4 — closes remaining Phase 2.3 items)

All four items session 3's audit identified as still open are now closed:

- **Context-window enforcement** — `src/routing/mod.rs` adds `Router::estimate_request_tokens` (chars/4 heuristic over message content, plus `max_completion_tokens`/`max_tokens`, since no tokenizer dependency exists in this project) and the Stage 4 filter now rejects a candidate whose `max_context_length` is exceeded with a distinct `"context_length_exceeded"` reason (previously folded into the generic `"missing_required_capability"`, and previously not checked at all). `docs/ROUTING_AND_POLICIES.md` updated to describe the real heuristic instead of "not currently enforced." Test: `tests/routing_tests.rs::test_context_length_filtering`.
- **`n > 1` (multiple choices)** — `src/providers/openai_compatible.rs` now iterates every entry in `choices` (streaming SSE and non-streaming body alike) instead of only `[0]`, emitting `ContentDelta`/`ToolCallDelta` events tagged with their real choice index; `src/server/routes/chat_completions.rs`'s non-streaming aggregation path now keys its accumulator by choice index and returns one `choices[]` entry per index. Scoped deliberately: reasoning is still only captured for choice 0 (`ReasoningDelta` has no per-choice index in the event model, and reasoning + `n>1` is not a common combination), and all choices in one attempt share the attempt's single `finish_reason` (the event model has no per-choice finish signal). The non-streaming JSON-parsing logic was factored out into `OpenAICompatibleAdapter::events_from_non_streaming_response` so it's unit-testable without a network round-trip. Tests: `tests/bugfix_tests.rs::test_multi_choice_non_streaming_response_yields_all_choices`, `tests/audit_contract_tests.rs::chat_completions_must_return_all_choices_for_n_greater_than_one`.
- **Full Responses-API SSE streaming** — `src/server/routes/responses.rs` now returns real `text/event-stream` output when `stream: true`: a `response.created` event, `response.output_text.delta` per content chunk, and a terminal `response.completed` (or `response.failed` if a mid-stream provider error occurred — surfaced to the client as its own `error` event first). This is a documented subset of the real Responses streaming protocol — no tool-call or reasoning-summary streaming events, no multi-item output — not full parity, and `docs/API_REFERENCE.md` now documents the actual event shapes and that scoping explicitly. Reuses `chat_completions::StreamGuard` (now `pub(crate)`) for the active-request/latency/`RequestCompleted` bookkeeping instead of duplicating it. Test: `tests/audit_contract_tests.rs::responses_must_stream_real_sse_when_requested`.
- **`reasoning.effort` extraction test** — added `tests/audit_contract_tests.rs::responses_must_extract_reasoning_effort_from_nested_object`, exercising the Responses-API-shaped `reasoning: {"effort": ...}` path specifically (previously only the flat-field path had coverage).

`cargo test --all-targets --offline` is green: **64 passed, 1 ignored, 0 warnings** across 8 test files (up from 59/1). `cargo check --all-targets --offline` after a forced rebuild shows zero warnings.

**Phase 2.3 is now fully closed.** No known gaps remain in that item beyond the documented scoping decisions above (reasoning + n>1, per-choice finish_reason, Responses streaming event coverage) — these are intentional, bounded simplifications, not defects, and are called out explicitly in both the code comments and the docs referenced above.

## Implementation status (updated 2026-09-22, session 3 — audit re-verification)

**No source changes landed between session 2 and this audit.** Git status/mtime inspection shows every tracked `src`/`tests`/`docs` file still carries the session-2 timestamps except four files (`src/types/request.rs`, `src/server/routes/responses.rs`, `src/providers/openai_compatible.rs`, `tests/provider_options_tests.rs`) touched by session 3's own work below. The only newer filesystem artifacts (`.developer_profile_analysis.md`, `DEV_PROFILE*.md`) are outputs of an unrelated third-party "developer profile builder" tool and contain no code or doc changes to this project. `cargo test --all-targets --offline` is green: **59 passed, 1 ignored, 0 warnings** across 8 test files (up from 58/1 — the delta is `tests/provider_options_tests.rs::test_previously_dropped_fields_are_forwarded`, added this session).

**Session 3 closed the remainder of Phase 2.3's field-forwarding gaps**: `Role::Developer` (deserializes and forwards as `"developer"` in the outbound body), `max_completion_tokens`, top-level `reasoning_effort`, and `parallel_tool_calls` are now `GatewayRequest` fields wired through `OpenAICompatibleAdapter::build_chat_body`; `modalities` (already a field) is now actually forwarded instead of accepted-and-discarded. `responses.rs` extracts `reasoning_effort` from both the Responses-API-shaped `reasoning.effort` object and the flat Chat-Completions-style field, and maps `"developer"`-role input items correctly. Verified via `tests/provider_options_tests.rs::test_previously_dropped_fields_are_forwarded`.

**Still open in Phase 2.3** (confirmed unchanged by re-reading the source, not just trusting prior notes):
- `n > 1` (multiple choices) — `src/providers/openai_compatible.rs` still only ever reads `choices[0]` in both the non-streaming body (`response_body["choices"][0]`, line ~355) and every streaming SSE delta field (lines ~537-594). A multi-choice request silently gets only the first choice.
- Context-window enforcement — `src/routing/mod.rs::is_compatible` (~line 393) checks tools/structured_output/reasoning/vision but never reads `candidate.max_context_length`; the Stage 4 filter has no token-estimation step at all. `docs/ROUTING_AND_POLICIES.md` already documents this honestly as "not currently enforced." This is the next actionable item if continuing Phase 2.3.
- Full Responses-API SSE streaming — `src/server/routes/responses.rs` now correctly passes the caller's real `stream` value to the *upstream* provider request, but the handler still always drains it server-side (`InstrumentedStream` + full accumulation) and returns one JSON object; a caller requesting `stream: true` gets no SSE response. Documented as a known gap inline (lines 106-109) and in the plan (2.2).
- No test exists yet for `responses.rs`'s `reasoning.effort` extraction path specifically (only the flat-field path is exercised via `provider_options_tests.rs`).

## Implementation status (updated 2026-09-21, session 2)

**Phase 0: done.** 0.1–0.6 all implemented and verified: transactional, correctly-ordered batch flushes with retry + JSONL fallback log (`src/telemetry/writer.rs`), gateway-generated `request_id` decoupled from client-suppliable `client_correlation_id` (migration `002_client_correlation_id.sql`, all three route handlers), telemetry writer flush-and-join on shutdown (`src/main.rs`, `src/telemetry/mod.rs`), and `GatewayConfig::validate()` rejecting zero batch/flush/pool sizes and unknown policy/fallback-trigger strings.

**Phase 1: functionally done, including embeddings.** Rather than extracting a shared `ExecutionEngine`, the fixes were applied directly at each call site: chat_completions.rs, responses.rs, **and now embeddings.rs** all emit `ProviderAttemptStarted`/`Completed` on every exit path (including pre-stream connection/HTTP failures and attempt timeouts), share the same retry/backoff and `fallback_on`-gated fallback logic, and enforce `attempt_timeout_secs`. Explicit-address routing enforces the `enabled` switch; a `RoutingDecision` persists even when routing selects zero candidates; the `RoutingPolicy` dispatch gap is caught by config validation. Also fixed in this pass: a genuine double-decrement of `active_requests` in `responses.rs`'s streaming path (A13), and two `record_request_end(..., false)` calls that were mislabeling error outcomes as non-errors. A dedicated `ExecutionEngine` module (deduplicating the now-three-times-repeated retry/telemetry code) is still worth doing as a refactor but isn't a correctness gap anymore — deprioritized.

**Phase 2: 2.1, 2.2, 2.5 done from session 1. Session 2 adds part of 2.3**: `UsageUpdate` is no longer dropped from the SSE stream (Chat Completions) or from the non-streaming response's usage details (both endpoints now include `prompt_tokens_details`/`completion_tokens_details`); Responses' usage now uses the Responses API's own `input_tokens`/`output_tokens` names instead of Chat Completions' names; `stop` now accepts a bare string as well as an array. **Still not done**: `n>1` (multiple choices), `developer` role, `max_completion_tokens`/`reasoning_effort` top-level passthrough, `modalities` forwarding, and full Responses-API SSE streaming output (Responses still collects the full response server-side and returns one JSON object even when the caller requests `stream: true` — only the *request* to the upstream provider honors `stream` now, not the client-facing response shape).

**Phase 3: all six items addressed to some degree.**
- **3.1** (timing model) — partially done: a new `ProviderEvent::FirstByte` is now emitted at the real first body byte and persisted into new `provider_attempts.first_byte_at`/`first_token_at` columns (migration `003_first_byte_and_first_token.sql`), closing the "no producer at all" half of this finding. Still not done: `Connected` still measures time-to-response-headers, not TCP/TLS establishment (documented honestly now rather than fixed, since reqwest doesn't expose a connection-establishment hook without a custom connector) — see `docs/ROUTING_AND_POLICIES.md`.
- **3.2** (reasoning visibility) — done: `writer.rs` distinguishes `count_only` from `none` in both event-arrival orders. DB-dependent, not covered by a unit test (same limitation as A02).
- **3.3** (operational metrics semantics) — done: health-check and inference latency are separate rolling averages; Chat Completions/Responses now call `record_latency` on success; `/metrics.gateway.total_requests` now counts actual requests via a new `AppState` counter instead of summing provider attempts; a genuine double-decrement bug in `responses.rs` is fixed.
- **3.4** (model/provider snapshot writers) — done: `TelemetryEvent::ModelSnapshot`/`ProviderSnapshot` are emitted once at startup and on every health-check tick respectively, persisted into the previously-writerless `model_snapshots`/`provider_snapshots` tables.
- **3.5** (cost accounting) — done: `GatewayConfig.pricing` (keyed by `"provider/model"`) is optional per-deployment pricing; when configured, `UsageRecorded` handling computes and persists a `cost_records` row. No entry means no row, never a fabricated zero.
- **3.6** (raw response capture) — partially done: `ProviderEvent::Completed` now carries the full raw response body for non-streaming requests (where a single body genuinely exists) through to `generations.raw_response`, gated by `store_raw_bodies` as documented. A real SSE stream still has no single body to capture (each chunk is already its own `stream_events` row).
- **3.7** (doc alignment) — done for everything touched this session: `docs/TELEMETRY_AND_DATABASE.md`'s table list/diagram/column names, `docs/ROUTING_AND_POLICIES.md`'s policy table (now correctly describes cumulative-mean latency, not EMA, and the context-window-enforcement gap), `docs/CONFIGURATION_GUIDE.md`'s new `pricing`/`fallback_log_path` sections, README's test counts and Rust version minimum.

Also fixed while addressing CL03 fully: `Router::route`'s policy dispatch now rejects (rather than silently downgrades) a request naming one of the five recognized-but-unimplemented `RoutingPolicy` variants (`local_first`/`cheapest`/`fastest`/`highest_quality`/`balanced`), and `GatewayConfig::validate()` restricts `routing.default_policy` to the three actually-implemented names so a config can never pass validation and then fail every request at runtime.

**Phase 4–5: not started.**

**Verification:** `cargo test --all-targets` is green (58 passed, 1 ignored, 0 warnings) across 8 test files, including two new ones added this session (`config_validation_tests.rs`, 12 tests covering Phase 0.6 and 3.5's validation logic). Of the 11 `#[ignore]`d contract probes that existed at the start of session 1, **10 now pass and are un-ignored**. Only the database-dependent A02 probe remains ignored — it requires a live disposable PostgreSQL instance not available in this environment. The reasoning-visibility (3.2), snapshot-writer (3.4), and cost-accounting (3.5) fixes share this same limitation (DB-dependent behavior, verified by code review and by the transaction/query logic compiling and type-checking against the real schema, but not exercised by an automated test here) and should be checked against a real database using the same disposable-instance approach the Codex audit used for A02.

---


This plan consolidates [`INIT_CONVERSATION_AUDIT.md`](INIT_CONVERSATION_AUDIT.md) (findings **A01–A18**) and [`INIT_CONVERSATION_AUDIT_CLAUDE.md`](INIT_CONVERSATION_AUDIT_CLAUDE.md) (findings **CL01–CL08**, plus three amendments to A09/A15/A18) into a single ordered set of work. It targets every issue raised in both documents. It does not re-litigate which findings are correct — both audits independently verified their claims against the source directly, and this plan treats them as one merged backlog, deduplicated where they describe the same defect.

## How this plan is organized

The two audits already converge on a dependency order (the Codex audit's "Remediation order" section, section CL08's reframing). This plan follows that shape:

1. **Phase 0 — Stop the silent data corruption.** Fixes that are pure bug fixes with no design decisions attached. Do these first because every later phase's testing depends on telemetry actually landing correctly, and because some of these are one-line/small-diff fixes.
2. **Phase 1 — Unify the execution lifecycle.** The attempt/error/cancellation model needs one shared implementation instead of three near-duplicate copies (chat completions, responses, embeddings), because CL04, A03, A04, and A12 all trace back to the same root cause: attempt bookkeeping is manually re-derived at each call site instead of owned by one component.
3. **Phase 2 — Make protocol/provider support truthful.** Fix concrete provider bugs (Gemini/HF URLs), decide what Chat Completions/Responses/Embeddings actually promise to support, and either implement or explicitly reject the rest.
4. **Phase 3 — Make telemetry analytically trustworthy.** Timing semantics, reasoning visibility, provenance/identity, cost accounting.
5. **Phase 4 — Build the exploration layer.** The analysis/presentation/visualization work the user was explicit about not wanting to skimp on (`INIT_CONVERSATION.md:659`), gated on Phases 0–3 because there is no point building a dashboard over data known to be unreliable.
6. **Phase 5 — Explicitly-deferred v2 work.** Durable outbox, circuit breakers, caching, learned routing (still out of scope per the user), OTLP export.

Each item lists: the finding IDs it closes, the files it touches, and what "done" looks like. Items inside a phase are mostly independent of each other unless noted; phases are sequential because later phases assume earlier ones are merged (tests from Phase 0 must stay green through later phases).

---

## Phase 0 — Stop the silent data corruption

Goal: no request, once it reaches telemetry, should have its data silently dropped, overwritten, or mislabeled. These are the fixes with the highest data-loss-per-line-changed ratio.

### 0.1 Fix stream-event insert ordering (A02, CL05)
- **Problem:** `TelemetryWriter::flush_batch` bulk-inserts all `StreamChunk` rows in a batch before processing `RequestReceived` rows in the same batch, so a request whose parent row and chunks land in the same flush window fails its entire chunk insert — and because it's one multi-row `INSERT`, unrelated requests' chunks in the same statement are dropped too (CL05).
- **Fix:** In `src/telemetry/writer.rs::flush_batch`, process `RequestReceived` (and any other event that creates/updates `inference_requests`) for the batch *before* the stream-chunk bulk insert, or switch the stream-event insert to per-row execution inside the same transaction as the parent upsert so a single bad row doesn't reject its batch-mates. Wrap the whole flush in one `sqlx::Transaction` (see 0.2) so ordering is enforced by dependency, not by code-review discipline.
- **Test:** extend `tests/audit_contract_tests.rs`'s database probe (or add a new one) asserting that N concurrent requests whose `RequestReceived` and `StreamChunk` events are queued into the same flush window all persist their full chunk set. Un-ignore it once fixed.

### 0.2 Make batch flush transactional (A01, part of A17)
- **Problem:** `flush_batch` executes each `sqlx::query` independently against the pool; a mid-batch failure leaves a partial write and the failed event is discarded (batch is `clear()`'d unconditionally at the end). Docs claim `BEGIN`/`COMMIT` batching that doesn't exist.
- **Fix:** Acquire one `sqlx::Transaction<'_, Postgres>` per flush, run all inserts/updates against it, `commit()` at the end. On error, roll back and retry the batch (see 0.3) rather than discarding it.
- **Test:** a probe that forces one query in a batch to fail (e.g. a constraint violation) and asserts no partial rows from that batch persist, and the batch is retried rather than dropped.

### 0.3 Stop discarding failed writes (A01)
- **Problem:** `flush_batch` calls `batch.clear()` unconditionally, so a failed `persist_event` call (logged via `tracing::error!`) permanently loses that event with no retry.
- **Fix:** On transaction failure, requeue the batch (bounded retry count with backoff) instead of clearing it. If retries are exhausted, write the batch to a local fallback file (see Phase 5.1 for the full outbox design — this can be a minimal version: append-only JSONL under a configurable path) rather than dropping it, so "Postgres temporarily unavailable" degrades to "delayed" rather than "lost," partially satisfying `INIT_CONVERSATION.md:1127-1139` ahead of the full outbox in Phase 5.
- **Test:** simulate a DB outage mid-run (drop the pool / point at an unreachable host) and assert events queued during the outage are eventually persisted once connectivity returns, or land in the fallback file.

### 0.4 Replace client-suppliable request ID as primary key (A15, CL amendment)
- **Problem:** `chat_completions.rs` trusts an inbound `x-request-id` header verbatim as the DB primary key; `RequestReceived`'s `ON CONFLICT (request_id) DO UPDATE` means a reused ID silently overwrites a prior unrelated request's `requested_model`/`canonical_messages`/`raw_request`.
- **Fix:** Generate the gateway's own UUIDv7 `inference_id` server-side, always, as the actual primary key for `inference_requests` and everything that references it. Keep the client-supplied `x-request-id` (if any) as a separate `client_correlation_id` column for idempotency/tracing purposes, not as a join key with upsert semantics. Requires a migration adding the column and changing the PK, plus updating every `writer.rs` insert and every route handler that currently conflates the two IDs.
- **Test:** a probe sending two distinct requests with the same `x-request-id` header, asserting both persist as distinct rows with distinct content.

### 0.5 Flush and join the telemetry writer on shutdown (part of A01)
- **Problem:** `main.rs`'s graceful shutdown drains HTTP connections but never signals or awaits the telemetry writer task or the health-check worker; process exit can abort an unflushed batch.
- **Fix:** Hold the writer's `JoinHandle` (currently discarded into `_writer_handle`), drop the `mpsc::Sender` on shutdown (closing the channel causes `TelemetryWriter::run`'s `None` branch to flush and return), then `.await` the handle before `main` returns. Also cancel/await the health-check worker's task handle.
- **Test:** an integration test that sends a request, immediately triggers shutdown, and asserts the request's telemetry row exists in the database afterward.

### 0.6 Fix zero-value config panics (A18 amendment — verify, then fix regardless)
- **Problem:** `TelemetryConfig::batch_size: 0` and `flush_interval_ms: 0` are not rejected by `config/mod.rs`, and are very likely to panic (`tokio::sync::mpsc::channel` requires `buffer > 0`; `tokio::time::interval(Duration::ZERO)` also panics).
- **Fix:** Add validation in `GatewayConfig::load`/`from_file` (or a new `GatewayConfig::validate()` called right after parsing) that rejects `batch_size == 0`, `flush_interval_ms == 0`, unknown `routing.default_policy` values, and unknown `routing.fallback_on` strings — return a clear `anyhow` error naming the bad field instead of deferring to a runtime panic or (per A18) a silently-dropped fallback trigger.
- **Test:** unit tests constructing a `GatewayConfig` with each invalid value and asserting `validate()` rejects it with a descriptive message.

**Phase 0 exit criteria:** `cargo test --all-targets` green; the previously-`#[ignore]`d probes in `tests/audit_contract_tests.rs` covering A01/A02/A15 pass and are un-ignored; a manual run against a real (disposable) Postgres reproduces zero data loss for the conversation's own worked fallback example (local failure → cloud success, both attempts fully recorded).

---

## Phase 1 — Unify the execution lifecycle

Goal: one component owns "try this candidate, record what happened, decide retry vs fallback vs give up" — not three copy-pasted loops in `chat_completions.rs`, `responses.rs`, and (partially) `embeddings.rs`.

### 1.1 Extract a shared attempt-execution loop (A03, A04, A12, CL04)
- **Problem:** The candidate/retry/fallback loop is duplicated near-verbatim across `chat_completions.rs` and `responses.rs` (both read in full for this audit — they are structurally identical), and `embeddings.rs` has its own simpler, non-equivalent version (no retry backoff, no attempt-timeout enforcement, no `ProviderAttemptStarted`/`Completed` telemetry at all). Pre-stream connection failures (`Ok(Err(e))` and the adapter's own `?`-propagated `reqwest` errors) never go through `InstrumentedStream`, so they never emit `ProviderAttemptStarted`/`Completed` (CL04) — this is why the conversation's own "local refused → cloud success" example isn't reconstructable today.
- **Fix:** Introduce an `ExecutionEngine` (or similar) in a new `src/execution/mod.rs` that takes a `RoutingPlan`, a `Provider` registry lookup, and a telemetry sender, and owns:
  - Emitting `ProviderAttemptStarted` **before** calling `provider.execute()`, not only inside `InstrumentedStream` after a stream exists. This closes CL04/A03 directly.
  - Emitting `ProviderAttemptCompleted` on every exit path — success, HTTP error, connection error, JSON error, and outer timeout — not only on `ProviderEvent::Completed`/`Error` inside a stream.
  - The retry-vs-fallback decision (currently duplicated fallback::should_fallback + retryable checks in each handler).
  - Returning a normalized `ExecutionOutcome` (stream handle, or complete response, or exhausted-with-last-error) that route handlers translate into their protocol-specific response shape.
- Update `chat_completions.rs`, `responses.rs`, and `embeddings.rs` to call this engine instead of their own loops. Embeddings gains attempt tracking, retry, and attempt-timeout enforcement it currently lacks (A14).
- **Test:** the existing `routing_tests.rs`/`fallback_tests.rs` suites should still pass unmodified in spirit (may need call-site updates); add a new integration test asserting identical `ProviderAttemptStarted`/`Completed` telemetry shape across chat completions, responses, and embeddings for the same simulated failure.

### 1.2 Fix stream failure/cancellation to preserve partial content (A04)
- **Problem:** `InstrumentedStream` only emits `GenerationContent` (the accumulated text) inside the `ProviderEvent::Completed` branch — an `Error` mid-stream, or the inner stream simply ending without a terminal event, loses all accumulated content and reasoning. Additionally, `chat_completions.rs`'s SSE `.chain(futures::stream::once(...))` unconditionally marks the `StreamGuard` as completed right before sending `[DONE]`, regardless of whether an `Error` event occurred upstream — the `RequestCompleted.success` flag can read `true` for a request that actually failed mid-stream.
- **Fix:**
  - Add a `Drop` impl or explicit finalizer on `InstrumentedStream` that emits `GenerationContent` with whatever was accumulated so far, on *any* terminal path (error, unexpected end, or normal completion) — not only inside the `Completed` match arm.
  - Have `event_to_sse` (currently `_ => None`, silently dropping `Error`) serialize `ProviderEvent::Error` as a client-visible SSE `error` chunk before `[DONE]`, per the canonical `StreamEventKind::Error` type that already exists in `src/types/response.rs` but is unused.
  - Fix the `[DONE]`-chain closure to only mark `completed = true` if a `Completed` event (or a non-retryable, already-non-fallback error) was actually observed, not unconditionally.
- **Test:** a probe provider that emits content then an `Error` mid-stream; assert (a) the client sees an `error` SSE chunk, (b) `generations.content` contains the partial text, (c) `inference_requests.success = false`.

### 1.3 Reject unauthorized fallback after retries exhausted (A12)
- **Problem:** `chat_completions.rs`/`responses.rs` fall through to the next candidate on `Ok(Ok(_))` (unexpected provider output) and on some retry-exhausted paths regardless of `fallback_on` configuration — `fallback_on: []` doesn't actually disable fallback in every branch.
- **Fix:** Route every failure branch through the same `fallback::should_fallback` check introduced in 1.1's `ExecutionEngine`, including the "unexpected provider output" and retry-exhausted cases, so `fallback_on: []` is authoritative everywhere.
- **Test:** a config with `fallback_on: []` and a candidate configured to fail in each of the distinguishable ways (timeout, unexpected output, retry-exhausted); assert none of them silently continue to the next candidate.

### 1.4 Enforce explicit-path constraints without removing explicit addressing's purpose (A11, CL02)
- **Problem:** Explicit-provider (`routing.provider`) and explicit-model (`provider/model` string) short-circuits in `Router::route` skip availability/health/capability filtering entirely, so `enabled: false` doesn't prevent execution, and no `RoutingDecision` is persisted when routing fails with zero candidates (CL02).
- **Fix:**
  - Add an administrative-only check (not a policy/priority check — the conversation's "explicit IDs enable reproducibility" intent should be preserved) that still rejects a candidate whose provider is `enabled: false` or unregistered, on both explicit paths. This is a validation step, not a policy decision — keep it separate so future readers don't conflate "explicit selection" with "no constraints at all."
  - Construct and emit a `RoutingDecision` (with `candidates_considered/rejected` populated) even in the `compatible.is_empty()` total-failure branch, before returning the error, so "why nothing was selected" is always persisted (closes CL02's second half).
- **Test:** a request with `routing.provider` pointing at a `enabled: false` provider must fail cleanly rather than execute; a request against a model with zero compatible candidates must still produce a persisted `RoutingDecision` row.

### 1.5 Wire the `RoutingPolicy` enum into the actual dispatch (CL03)
- **Problem:** `Router::route`'s policy `match` operates on raw config strings and only recognizes two of the eight `RoutingPolicy` enum variants; the other six (including `local_first`, which is one of the conversation's named v1-adjacent policies) silently become ordered routing with no telemetry marker distinguishing requested-vs-applied policy.
- **Fix:** Parse the config string into `RoutingPolicy` once (via `serde` or a `FromStr` impl) at config-validation time (see 0.6), reject unknown policy names outright rather than silently falling back, and either (a) implement `local_first`/`cheapest`/`highest_quality`/`balanced` as real dispatch arms, or (b) if deferring their implementation past this remediation pass, have `Router::route` explicitly reject a config that requests them with a clear "not yet implemented" error rather than silently substituting ordered routing. Silent substitution is the actual defect; deferring the policies themselves is fine per the conversation's own v1 scope.
- **Test:** a config requesting each of the six currently-unimplemented policy names should either work correctly or fail loudly at startup — never silently downgrade.

**Phase 1 exit criteria:** the conversation's own worked example (`INIT_CONVERSATION.md:914-935`) is fully reconstructable from the database for a real local-failure/cloud-fallback run; `fallback_on: []` is authoritative across all three route handlers; stream errors are visible to clients and correctly recorded.

---

## Phase 2 — Make protocol/provider support truthful

Goal: every documented capability either works as documented or is explicitly rejected/marked unsupported — no silent contract changes.

### 2.1 Fix provider endpoint construction (A10, CL07)
- **Problem:** `OpenAICompatibleAdapter::endpoint()` only strips a redundant `v1/` when `base_url` already ends in `/v1`; Gemini's configured base (`.../v1beta/openai`) doesn't match that check, so every Gemini request hits `.../v1beta/openai/v1/chat/completions` (confirmed independently in both audits by reading the file and the config together). Hugging Face's example base URL is also stale per current HF routing docs (A10).
- **Fix:** Make endpoint joining provider-aware: either let each adapter supply its own path template instead of a single global suffix-stripping heuristic, or make the heuristic check for `/v1` **anywhere as a path segment**, not only as a literal suffix. Update `config/gateway.yaml`'s example Hugging Face `base_url` to the current documented router endpoint.
- **Test:** a unit test per configured provider base URL (llama.cpp, OpenRouter, vLLM, Gemini, Ollama, Ollama Cloud, Hugging Face) asserting the constructed `/v1/chat/completions`, `/v1/models`, and `/v1/embeddings` URLs match each provider's actual documented endpoint shape. This is cheap to write and should have existed already — do it once per provider, not just for Gemini.

### 2.2 Decide and enforce the Responses API's real contract (A09, CL amendment)
- **Problem:** `responses.rs` (already improved since the Codex audit re: retry/fallback per the Claude audit's review) still hardcodes `stream: false` regardless of the caller's request, silently drops `tools`/`reasoning`/`response_format`/`provider_options`/`routing`, and only extracts plain-string `input` array items (silently dropping object-form message items, which is the standard shape per OpenAI's Responses API).
- **Fix:** Make an explicit choice and implement it fully rather than partially:
  - **Option A (recommended for effort/value):** Formally scope Responses as a documented subset (text input/output only, no tools/streaming) and make the handler **reject** requests using unsupported fields (`400` with a clear message) instead of silently discarding them. Update `docs/API_REFERENCE.md` to match reality.
  - **Option B:** Implement full parity — object-form input items, `stream: true` support (SSE per the Responses streaming spec), tool/reasoning passthrough. Larger effort; only worth it if Responses API traffic is expected to matter for this user's actual usage (mostly OpenRouter/Chat Completions per the conversation's opening message — confirm before investing here).
- **Test:** whichever option is chosen, add a test asserting the documented behavior — either successful full-fidelity round-trip (Option B) or a clean rejection (Option A) for each currently-silently-dropped field.

### 2.3 Fix Chat Completions protocol gaps (A08)
- **Problem:** No `stream_options`/streamed-usage support; `UsageUpdate` events are discarded in `event_to_sse`; only `choices[0]` is ever read even when `n > 1`; string-valued `stop` fails to deserialize; `developer` role, `max_completion_tokens`, `reasoning_effort`, `parallel_tool_calls` are unsupported; `modalities` is accepted but never forwarded.
- **Fix:** Prioritize by actual usage impact for this user's stack (per the conversation, OpenRouter and local llama.cpp/vLLM dominate):
  1. Forward `UsageUpdate` into the final SSE chunk (cheap, high value — usage-per-request is one of the explicit metrics the conversation wants).
  2. Support `n > 1` by iterating all `choices`, not just index 0 (moderate effort, needed for "all paid-for completions" to actually reach the client and telemetry).
  3. Accept string-valued `stop` (`Option<StopSequence>` custom deserializer accepting both a string and an array).
  4. Add `developer` role and `max_completion_tokens`/`reasoning_effort` passthrough into `GatewayRequest`.
  5. Either forward `modalities` to providers that support it or drop the field from the request schema until it's wired up — don't accept-and-ignore.
- **Test:** contract tests per fixed field, following the existing pattern in `tests/bugfix_tests.rs`.

### 2.4 Bring embeddings into the shared execution/telemetry pipeline (A14)
- Covered structurally by Phase 1.1 (embeddings uses the same `ExecutionEngine`). Additionally: propagate `dimensions`/`encoding_format`/provider-specific options through `Provider::embeddings` (currently only `model`/`input` are accepted), and check deployment-level (not provider-wide) embedding capability.
- **Test:** an embeddings request with `dimensions` set against a provider that supports it, asserting the parameter reaches the upstream request body.

### 2.5 Reasoning controls: connect request to provider (A07 request-side half)
- **Problem:** `GatewayRequest.reasoning` participates in capability filtering but `build_chat_body` never sends it to any provider.
- **Fix:** In `OpenAICompatibleAdapter::build_chat_body` (or per-adapter overrides where the wire format differs, e.g. Gemini's nested `thinkingConfig`), map `ReasoningConfig{enabled, max_tokens, effort}` to each provider's actual reasoning-control field. This is provider-specific — llama.cpp/vLLM may have no equivalent (fine, no-op), OpenRouter/Anthropic-style APIs use `reasoning.effort`/`reasoning.max_tokens`, Gemini uses `thinkingConfig.thinkingBudget`.
- **Test:** per-provider unit test asserting a `reasoning.enabled = true` request produces the provider-correct field in the outbound body.

**Phase 2 exit criteria:** every provider in `config/gateway.yaml` has a passing endpoint-shape test; Chat Completions/Responses/Embeddings each have an explicit, tested, documented contract — no field is silently accepted and discarded.

---

## Phase 3 — Make telemetry analytically trustworthy

Goal: the questions the conversation explicitly wants answered (`INIT_CONVERSATION.md:306-322`) are actually answerable from the database with correct semantics.

### 3.1 Fix timing model (A06)
- Distinguish `Connected` (should measure TCP/TLS establishment) from "response headers received" (currently conflated — `Connected` is stamped after `reqwest.send().await` returns, which is after the full request/response header round-trip, not connection establishment).
- Wire up `FirstProviderByte` (currently no producer at all) and stop no-op'ing it and `FirstGenerationChunk` in `writer.rs::persist_event` — persist both.
- Use `std::time::Instant`/monotonic durations for all `duration_us`/latency computations instead of wall-clock `DateTime<Utc>` subtraction (`stream_instrumentation.rs` and route handlers currently subtract `Utc::now()` timestamps, which is not monotonic-safe across NTP adjustments — acceptable for a personal single-host gateway but worth fixing since it's cheap and the conversation cares about precision here).
- For non-streaming responses, do not synthesize `ContentDelta`/etc. events with timestamps that imply real-time generation — mark them explicitly as "reconstructed from non-streaming response" so downstream TTFT/TPS queries can exclude them rather than silently averaging fabricated timing into real streaming metrics.
- **Test:** an integration test with a fake provider emitting known-delay chunks, asserting `TTFT`/`inter_chunk_latency` computed from persisted `stream_events` matches the injected delays within a tolerance.

### 3.2 Fix reasoning visibility end-to-end (A07 write-side, CL06)
- Construct a real `ReasoningOutput`/`ReasoningVisibility` value in `InstrumentedStream`/`writer.rs` instead of the current two-state `"exposed"`/`"none"` string: `CountOnly` when reasoning tokens are reported but no text arrived, `SummaryOnly` when a provider-specific summary field is present without full content, `Unavailable` vs `None` distinguishing "provider doesn't expose reasoning for this model" from "reasoning wasn't requested."
- **Test:** three fixture providers simulating each visibility state; assert `generations.reasoning_visibility` matches.

### 3.3 Fix operational metrics semantics (A13)
- Stop feeding health-check `/models` latency into the same rolling average used for `lowest_latency` routing and `/metrics.avg_latency_ms` — track health-probe latency and inference-attempt latency as separate counters in `ProviderRegistry`.
- Call `record_latency` from Chat Completions and Responses (currently only Embeddings does), so `lowest_latency` policy (once implemented per 1.5) has real inference-latency data to route on.
- Fix `/metrics.gateway.total_requests` to count incoming requests, not provider attempts; include routing failures.
- Fix the double-decrement bug in `responses.rs` where the active-request counter can be decremented once when a stream ends and again on stream error.
- **Test:** assert `/metrics` request counts match actual request volume in a scripted multi-request, multi-failure test run.

### 3.4 Populate `model_snapshots`/`provider_snapshots` or remove them (A15/CL08 table-cross-check)
- **Problem:** both tables exist in the migration with no writer anywhere — confirmed by grepping `CREATE TABLE` against every `INSERT INTO` in `writer.rs`.
- **Fix:** Either implement a snapshot writer (capture the resolved config/model/provider version at attempt time, so provenance survives config changes — this directly serves the "actual reported cost"/"context utilization" and future dataset-provenance goals), or remove the tables from the migration and documentation until they're implemented. Don't leave dead schema — it actively misleads anyone reading the schema as documentation.

### 3.5 Implement cost accounting or remove the scaffold (A15/A17 cost gap)
- `CostRecorded`/`cost_records` exist with no producer. Decide: implement a pricing table (static YAML mapping provider/model → $/1K tokens, computed from `generation_usage` at attempt-completion time) or explicitly document cost accounting as unimplemented rather than implying it works (the README currently claims cost tracking — A17).
- **Test:** given a known token count and a configured price, assert `cost_records.estimated_cost` matches the expected calculation.

### 3.6 Preserve raw provider payloads (A05)
- Capture the exact outbound request body per attempt (not the re-serialized `GatewayRequest`) and the raw response body/headers per attempt, gated behind `store_raw_bodies` (already exists as a config flag but isn't fully honored — `raw_response` is hardcoded to `None` in `stream_instrumentation.rs`).
- **Test:** with `store_raw_bodies: true`, assert `generations.raw_response` is non-null and matches the actual upstream payload for a fixture provider.

### 3.7 Align documentation with verified behavior (A17)
- Once 0.2/3.1–3.6 land, do a documentation pass: `docs/TELEMETRY_AND_DATABASE.md`'s table list and diagram (fix `client_disconnects`/`evaluations`/`dataset_entries` → `model_snapshots`/`provider_snapshots`, whichever exist by the end of this phase), `docs/ROUTING_AND_POLICIES.md`'s context-enforcement/EMA-latency claims, `docs/ARCHITECTURE.md`'s UUIDv7-everywhere and `GatewayResponse`-is-used claims, README's test count and Rust-version minimum. This should be a fast pass once the underlying behavior is fixed and stable — don't do it earlier, or it'll need repeating.

**Phase 3 exit criteria:** every question in `INIT_CONVERSATION.md:306-322` has a working, tested SQL query (add them to `docs/TELEMETRY_AND_DATABASE.md` alongside the existing examples) that returns correct results against a seeded test database; no `docs/*.md` file makes a claim contradicted by a passing test.

---

## Phase 4 — Build the exploration layer

Goal: satisfy `INIT_CONVERSATION.md:659`'s explicit instruction not to skimp on "data/metrics/analysis/data presentation/visualization." Gated on Phases 0–3 because building this over unreliable data wastes the effort and produces misleading dashboards.

This phase is necessarily higher-level than Phases 0–3 since no code exists yet; scope it as its own planning pass when reached, but the shape per the conversation's own recommended split (`INIT_CONVERSATION.md:730-781`) is:

1. **Rust analytics API** (thin layer over the now-trustworthy Postgres schema): request/attempt/timeline query endpoints, comparison queries (per-provider TTFT/TPS, fallback frequency — the sample SQL in `docs/TELEMETRY_AND_DATABASE.md` is a reasonable starting point once Phase 3 makes it accurate).
2. **Next.js/TypeScript dashboard**: request explorer, timeline viewer (visualizing the per-chunk timestamps Phase 3.1 makes trustworthy), filters/search/export.
3. **Python analysis companion** (as needed, not v1): heavier statistical analysis, dataset curation (accepted/rejected pairs, SFT/DPO export) building on the raw+normalized+derived data model the conversation describes (`INIT_CONVERSATION.md:973-1016`).

Do not start this phase's detailed design until Phase 3 is merged — re-derive the actual schema and query needs at that point rather than planning against today's schema, since Phase 3 changes it materially (new columns, new tables possibly removed/added per 3.4/3.5).

---

## Phase 5 — Explicitly-deferred v2 work

These were called out by the conversation itself as later-stage, or are the Codex audit's "step 6." Do not pull these forward; they're listed for completeness and to make sure nothing is lost.

1. **Durable local journal/outbox** (`INIT_CONVERSATION.md:1113-1139`) — a proper implementation beyond Phase 0.3's minimal fallback-file stopgap: a write-ahead local log with checkpointing, so the telemetry pipeline survives extended Postgres outages without any risk of loss, and can replay on recovery.
2. **OTLP export** — conversation explicitly frames this as an export mechanism layered on top of the authoritative Postgres records, not a replacement (`INIT_CONVERSATION.md:1073-1076`). Only worth doing once Phase 3's data is trustworthy enough to be worth exporting.
3. **Circuit breakers, provider health scoring beyond the current binary healthy/unhealthy, rate limiting, `max_concurrent` enforcement.**
4. **Prompt/semantic caching.**
5. **Cost/local-first/quality/balanced/cheapest routing policies** — the *dispatch mechanism* for these is fixed in Phase 1.5 (no more silent substitution), but implementing their actual selection logic (real per-token pricing feeding `cheapest`, a real quality proxy for `highest_quality`) is v2 per the conversation's own scope list.
6. **Dataset candidate/annotation/preference-pair workflows and SFT/DPO/evaluation export** — natural continuation of Phase 4.3's Python layer, but the conversation frames this as downstream of "analysis / filtering," i.e. after Phase 4 exists, not concurrent with it.
7. **Learned/AI-driven routing** — the user was explicit and unambiguous: "I am only interested in deterministic routing currently" (`INIT_CONVERSATION.md:661`). Nothing in this plan should introduce this; it's listed only so it's never accidentally treated as in-scope by a future contributor skimming `INIT_CONVERSATION.md`'s earlier speculative section about a "learned router."

---

## Cross-reference index

| Finding | Phase | Item |
|---|---|---|
| A01 | 0 | 0.2, 0.3, 0.5 |
| A02 | 0 | 0.1 |
| A03 | 1 | 1.1 |
| A04 | 1 | 1.2 |
| A05 | 2, 3 | CL01 note, 3.6 |
| A06 | 3 | 3.1 |
| A07 | 2, 3 | 2.5, 3.2 |
| A08 | 2 | 2.3 |
| A09 | 2 | 2.2 |
| A10 | 2 | 2.1 |
| A11 | 1 | 1.4 |
| A12 | 1 | 1.1, 1.3 |
| A13 | 3 | 3.3 |
| A14 | 1, 2 | 1.1, 2.4 |
| A15 | 0, 3 | 0.4, 3.4 |
| A16 | 4 | Phase 4 (entire) |
| A17 | 3 | 3.7 |
| A18 | 0, 1 | 0.6, 1.5 |
| CL01 | 2 | 2.1 (ResponseHeaders note folded in) |
| CL02 | 1 | 1.4 |
| CL03 | 1 | 1.5 |
| CL04 | 1 | 1.1 |
| CL05 | 0 | 0.1 |
| CL06 | 3 | 3.2 |
| CL07 | 2 | 2.1 |
| CL08 | — | Framing/priority note, not a standalone fix |

## Suggested execution note

Each phase should land as its own PR/branch with `cargo test --all-targets` green and, where a phase's item references `tests/audit_contract_tests.rs`, that file's relevant probes moved out of `#[ignore]` in the same PR — an ignored probe that stays ignored after its fix lands is a silent regression risk. Re-run the Codex audit's disposable-PostgreSQL check (§"Verification performed") at the end of Phase 0 and again at the end of Phase 3 to confirm the data-loss findings are actually closed under a real database, not just under mocked unit tests.
