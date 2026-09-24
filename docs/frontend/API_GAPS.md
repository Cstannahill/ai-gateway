# Read API gaps for the frontend

Source review: 2026-09-23. These are **proposed contracts**, not registered routes. The frontend never calls them. Existing backend files were not changed. Source evidence takes precedence over older audit findings and simplified examples.

## 1. Historical summary and series

**UI need:** a date-aware usage/activity view with comparable populations.

**Evidence:** `src/server/routes/analytics.rs` only accepts model/provider/success/limit/offset for request lists. `/metrics` holds process counters. There is no date-aware aggregate route.

**Proposed:** `GET /v1/analytics/summary?from=<UTC>&to=<UTC>&bucket=minute|hour|day&model=&provider=` returns `{scope:{from,to,bucket,provider_semantics},requests:{total,successful,failed,pending},attempts:{total,failed},usage:{prompt,completion,cached,reasoning,known_requests,eligible_requests},series:[...]}`. Use a half-open `[from,to)` interval, UTC bucket boundaries, bounded maximum buckets and explicit final-serving-provider semantics. Unknown usage is null, never zero; aggregate at request/attempt grain before joining.

**Acceptance:** seed boundary timestamps, retries, failed and pending requests, absent usage and repeated updates. Verify correct grain, UTC boundaries and coverage without multiplying requests or summing cumulative usage.

## 2. Usage observation identity and finality

**UI need:** trustworthy request totals, cached share, and timing sample counts.

**Evidence:** `UsageRecorded` always inserts in `src/telemetry/writer.rs`. `stream_instrumentation.rs` emits every usage update and converts absent primary counts to zero. Detail returns only attempt number and numeric fields; no timestamp, observation ID, final marker, or raw-provider usage. Multiple rows can be cumulative updates, not separate generations. This is a current source limitation, not merely legacy telemetry.

**Proposed detail extension:** usage rows include `{id,attempt_number,observed_at,kind:delta|cumulative|final,source,counts:{prompt:number|null,completion:number|null,total:number|null,cached:number|null,reasoning:number|null}}`; response includes an independently derived canonical final record per attempt and quality/coverage flags. Preserve raw observations.

**Acceptance:** duplicate/reordered cumulative updates and missing final updates never inflate totals; zero remains distinguishable from missing; invalid subsets are flagged. Until then the frontend displays observations separately and does not sum them.

## 3. Comparison grain, duration, sample counts and percentiles

**UI need:** defensible attempt volume/success, duration, TTFT and throughput comparison.

**Evidence:** comparison uses `LEFT JOIN generation_usage` before `COUNT(*)` and all averages. Multiple usage records multiply an attempt, inflate counts and change weighting. Duration/TPS exclude synthetic timing and unsuccessful attempts; TTFT excludes synthetic and null markers but includes failures with a first-generation marker. No timing sample counts exist. TTFT is `(first_token_at - provider_attempts.timestamp)`. Streaming duration starts when `InstrumentedStream` is constructed **after** `execute()` returns, so it is not full dispatch-to-completion latency. The frontend labels all values as reported and exposes these caveats; it does not reaggregate averages or rank quality.

**Proposed extension to existing comparison:** one row per provider/model with attempt-grain counts; `{duration:{mean_ms,sample_count,definition},ttft:{mean_ms,p50_ms,p95_ms,sample_count,definition},output_throughput:{mean,sample_count,denominator},synthetic_attempts,usage_coverage}`. Select canonical usage before joining. Preserve failure-inclusive TTFT semantics explicitly or change them with a versioned definition. A true end-to-end duration needs an aligned monotonic dispatch boundary.

**Acceptance:** two attempts, one with three usage observations, still count as two. Synthetic and missing timing are excluded per metric, counts reflect each metric's actual population, and percentiles come from eligible raw observations rather than averages.

## 4. Cost and pricing coverage

**UI need:** estimated charges, provider/model breakdown, pricing coverage and provenance.

**Evidence:** `cost_records` has a writer with configured per-1K prompt/output prices, but no read route exposes it. A missing row does not prove zero price. Current usage finality issues also affect correctness of totals.

**Proposed:** `GET /v1/analytics/costs?from=&to=&bucket=&provider=&model=` returns scoped `{currency,estimated_token_charge,billed_cost:null,priced_attempts,unpriced_attempts,eligible_attempts,pricing_source,pricing_version,series,breakdowns}`. Explicit configured zeros count as priced with a zero estimated charge; missing prices yield unknown cost. No fabricated cached-token discounts. Do not call local inference free or estimated charges provider bills.

**Acceptance:** absent price, explicit zero, positive price, different currencies and unknown usage yield distinct states. Repeated usage updates do not create repeated charges in the aggregate.

## 5. Request-detail provenance and timeline boundaries

**UI need:** route rationale, structured errors, choice-separated content and accurate attempt spans.

**Evidence:** detail omits routing decisions/rejections, cost records, separate error rows/tags, attempt start/end timestamps, generation IDs/choice IDs and raw response bodies. `GenerationContent` currently combines content deltas at attempt level, so a read-only addition alone cannot recover missing choice identity. The timeline does not have pagination. The frontend preserves each returned generation row and annotates this limitation.

**Proposed extension:** `attempts` include `started_at,finished_at,duration_definition`; `generations` include stable ID, choice index when captured and terminal/partial status; separate `routing`, `errors`, `costs` and their provenance. `GET /.../timeline?after=<cursor>&limit=1000` returns `events,next_cursor,complete` with stable ordering and original timestamps. Do not infer TCP connection time from response-header time.

**Acceptance:** local failure → cloud recovery shows both spans and rejection evidence; concurrent choices remain distinguishable; cancelled output remains partial; paginated events contain each original sequence exactly once. Missing capture stays explicit. Frontend currently bounds responses at 8 MiB, displays 100 exact events/page and exports the complete returned timeline within that bound.

## 6. Request-list pagination and filters

**UI need:** stable browsing under concurrent inserts and real date/application/tag search.

**Evidence:** list orders only by `received_at DESC` with offset pagination and no total. Supported filters are exact requested/resolved model, final serving provider, and success. The UI keeps visible pages stable and polls a separate first-page query for a new-row notification; no undocumented filters are sent.

**Proposed extension:** bounded keyset pagination ordered by `(received_at DESC, request_id DESC)` with `{requests,next_cursor,as_of}` plus UTC from/to, an allowlist of sort fields, optional application/task/tag filters with documented exact-match semantics. An attempted-provider filter must be separate from serving-provider.

**Acceptance:** equal timestamps and concurrent new inserts neither duplicate nor omit rows during a cursor traversal; filters are honored in queries, invalid sorts rejected, and pending requests remain distinguishable from failure.

## 7. Recovery endpoint scope

**UI need:** reliable recovered-request samples and separately scoped reliability rates.

**Evidence:** `/fallbacks` uses `LIMIT 200` **after joining failed attempts**. This caps rows, not distinct requests, despite the API documentation's description. A request may be cut off at the boundary; statuses other than success, including unfinished rows, qualify. No attempt number or received-at timestamp is returned. Distinct request count within the returned sample is safe; overall failure/fallback rate is not available.

**Proposed extension:** select 200 parent requests first, then return their full attempt chain with `sample_scope`, `truncated`, timestamp, attempt number and status. Keep unrecovered failures and fleet denominators in an independent summary contract.

**Acceptance:** 200 requests with several failed attempts return complete chains, not 200 joined rows. Same-provider retry, same-provider model switch, provider switch and unrecovered error have distinct counts.

## 8. Persistence health and authentication

**Evidence:** `/health` reports provider status, treating unknown health as acceptable. It does not query telemetry persistence. The writer's fallback replay is not a pre-write journal. No auth middleware is registered in `src/server/mod.rs`; optional Authorization examples are not evidence of authentication. A missing analytics pool returns 503; a query failure with an existing pool returns **500 analytics_query_error**, not necessarily 503.

**Proposed:** a read-only persistence status contract with explicitly observed writer state, last acknowledged flush, pending/dropped counts and observation timestamp. Any public deployment needs a separately implemented access-control boundary.

**Acceptance:** healthy inference with unavailable persistence remains distinguishable; query failure is not presented as no traffic; unknown writer state is not a fabricated capture guarantee. The added frontend binds to loopback by default and does not start, reconfigure or publish the Rust gateway.
