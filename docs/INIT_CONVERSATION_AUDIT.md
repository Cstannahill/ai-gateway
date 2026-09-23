# Audit against INIT_CONVERSATION.md

Audit date: 2026-09-21 (America/Chicago). Base commit: `098177173ab174838b38457fdbbd07e6b6310867`.

**Verdict: the repository implements a useful Rust gateway foundation, but does not yet fulfill the requested permanent inference-data platform. Even the narrower gateway v1 has correctness gaps. The highest-risk defects lose observations or report failed generations as successful.**

This audit covers the current working tree, including pre-existing edits to `src/routing/mod.rs`, `src/routing/policy.rs`, and `src/server/routes/responses.rs`. Those edits were preserved. No production implementation was changed. The additions are this report and explicitly ignored contract probes in [audit_contract_tests.rs](S:/Code/RustProjects/AI-Gateway/tests/audit_contract_tests.rs).

## Scope and interpretation

The conversation is an architectural discussion, not a finalized acceptance specification. Its statements have different weights:

- **Explicit user priorities:** PostgreSQL-backed permanent capture of requests, responses, exposed thinking, timings and tokens; serious analysis/presentation/visualization; deterministic routing only. See [INIT_CONVERSATION.md:655](S:/Code/RustProjects/AI-Gateway/INIT_CONVERSATION.md:655).
- **Proposed v1:** Chat Completions, Responses, aliases/capabilities, six main provider integrations, SSE, errors/usage, retries/fallback, database telemetry, health/provider/model/metrics endpoints. See [INIT_CONVERSATION.md:503](S:/Code/RustProjects/AI-Gateway/INIT_CONVERSATION.md:503).
- **Proposed architecture:** distinct canonical types, event-based providers, observations before aggregates, attempt history, raw data, reasoning visibility, and database writes outside stream delivery. See [INIT_CONVERSATION.md:785](S:/Code/RustProjects/AI-Gateway/INIT_CONVERSATION.md:785).
- **Explicit later work:** durable journal/outbox, sophisticated deterministic policies, circuit breakers, caching, evaluation/dataset tooling, and additional modalities. These are marked missing/deferred, not all treated as v1 blockers. AI routing is excluded by the later user clarification.

The user's later emphasis on analysis and visualization supersedes the earlier suggestion to keep those out of initial scope as an overall project goal. There is no agreed phase plan establishing when those layers must ship, so their absence is a major scope gap rather than evidence of a missed deadline.

Status meanings: **Implemented** = executable implementation exists for the stated behavior; **Partial** = only a subset works; **Incorrect** = observable behavior violates its contract; **Scaffold only** = types/schema/comments exist without a producing execution path; **Missing/deferred** = no implementation found. Implemented does not mean all live providers were certified.

## Verification performed

| Check | Result |
|---|---|
| Read conversation, all production Rust modules, migration, config, existing tests, and project documentation | Completed |
| `cargo test --all-targets --offline` before audit additions | **27 passed** |
| `cargo check --all-targets --offline` | Passed |
| Added contract probes, explicitly run with `--ignored` | **11 failed at intended contract assertions**, reproducing defects below |
| Actual PostgreSQL 18.3 migration and writer probe | Migration succeeded; **10 request rows, 10 generation rows, 0 expected stream-event rows** |
| Provider API references | Checked official Gemini, Hugging Face and OpenAI documentation |

The database check used an isolated disposable cluster under `target/audit-pg`, bound to `127.0.0.1:18999`, with no production credentials. It was shut down after the check. Files remain under ignored `target/` for inspection. No paid/live inference requests were sent, and the user's `.env` contents were not read. Live provider model availability, provider authentication, throughput, crash recovery, and long-duration load behavior were not certified.

The probes assert intended behavior and are ignored in the normal suite because this is an audit, not a remediation change. Their failures are findings, not passing validation. To reproduce the ten non-database probes:

```powershell
cargo test --test audit_contract_tests --offline -- --ignored --skip database_batch
```

For all eleven, first supply `AUDIT_DATABASE_URL` pointing only at an isolated disposable PostgreSQL database, then run:

```powershell
cargo test --test audit_contract_tests --offline -- --ignored
```

## Priority findings

P1 = blocks core correctness or data retention. P2 = incomplete behavior, misleading metrics, or missing functionality. Evidence is labeled **reproduced**, **database reproduced**, or **source verified**; no live-provider result is implied.

### A01 — P1: telemetry has no reliable delivery guarantee

**Incorrect for the permanent-record objective; source verified.** Both [AppState::emit_telemetry](S:/Code/RustProjects/AI-Gateway/src/server/mod.rs:35) and [stream instrumentation](S:/Code/RustProjects/AI-Gateway/src/telemetry/stream_instrumentation.rs:72) use `try_send`, dropping events when the channel fills. Closed-channel failures are not reported. This applies to content and lifecycle events, not only optional detailed chunk metrics.

[TelemetryWriter::new](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:34) permanently chooses log-only operation when its initial connection fails; it does not reconnect later. These logs contain event type/request ID, not the full observations. Migration failures only warn. [flush_batch](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:120) clears failed writes instead of retrying them. Writes are not transactional despite documentation.

[Shutdown](S:/Code/RustProjects/AI-Gateway/src/main.rs:177) drains HTTP connections but never closes and awaits the telemetry writer. The background health worker retains application state and a sender; the pipeline keeps its own sender. Runtime shutdown can abort an unflushed batch even after an otherwise graceful server stop.

**Needed:** explicit persistence guarantees, recoverable ingestion/replay or a fail-closed mode, ordered transactions, write acknowledgments/retries, drain-and-join shutdown, and visible persistence health. The conversation allows deferring the *journal implementation*, but does not make silent loss compatible with permanent capture.

### A02 — P1: bulk stream inserts run before their parent requests

**Incorrect; database reproduced.** [writer.rs:125](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:125) inserts every chunk in a batch before processing `RequestReceived`. `stream_events.request_id` has an immediate foreign key to `inference_requests`. If a request and its chunks share a batch, the bulk insert fails; one missing parent rejects that entire bulk statement, potentially losing chunks for other requests too. The batch is then cleared.

The isolated PostgreSQL probe persisted ten requests and ten generations but zero of ten chunks. This is data loss with a healthy database, not merely an outage scenario.

**Needed:** insert/upsert parent rows first, preserve dependencies within a transaction, and retry failures without losing or duplicating observations.

### A03 — P1: the failed-attempt history is largely absent

**Incorrect; HTTP-error path reproduced, other paths source verified.** Attempts are recorded only when `InstrumentedStream` consumes a provider `Started` event. HTTP errors returned as `ProviderOutput::Complete(Error)`, connection/JSON errors returned as `Err`, and outer timeouts bypass that wrapper. See [chat_completions.rs:140](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:140), [chat_completions.rs:342](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:342), and [responses.rs:164](S:/Code/RustProjects/AI-Gateway/src/server/routes/responses.rs:164).

Consequently, the requested “local connection refused → successful cloud fallback” history cannot reliably be reconstructed. Early routing and nonretryable-error returns also omit `RequestCompleted`. Invalid JSON is rejected before request telemetry. A failed all-candidates routing decision is returned as an error and its rejection details are not persisted.

**Needed:** record attempt start before executing the provider and finalize it on every exit path; persist request rejection/failure and failed routing decisions independently of whether a provider stream exists.

### A04 — P1: stream failures are hidden and partial generations disappear

**Incorrect; reproduced.** [event_to_sse](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:431) ignores `ProviderEvent::Error`. The chained end event [unconditionally marks completion](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:188) before emitting `[DONE]`. A provider stream emitting content and then an error returned only content plus `[DONE]`; its request was recorded as successful.

[InstrumentedStream](S:/Code/RustProjects/AI-Gateway/src/telemetry/stream_instrumentation.rs:205) emits accumulated generation content only on `Completed`. Error, unexpected EOF, and client cancellation do not preserve partial text/reasoning/tool arguments. There is no drop finalizer for attempts. The adapter also silently skips malformed JSON or upstream error-shaped SSE objects, and does not require a terminal provider event before EOF.

**Needed:** an explicit stream terminal-state machine, client-visible error events, accurate failure/cancellation records, and preservation of partial content on all terminal paths. Avoid retrying a generation after client-visible output has already begun; pre-output failures can be handled separately.

### A05 — P1: raw observations are not actually preserved

**Partial/incorrect; inbound-field loss reproduced, remaining paths source verified.** Chat deserializes directly into `GatewayRequest`, then serializes it as `raw_request` ([chat_completions.rs:52](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:52)). Unknown client fields are already discarded and gateway defaults have been added. It is neither the original client JSON nor the exact transformed provider request.

The sole generation producer sets [raw_response to None](S:/Code/RustProjects/AI-Gateway/src/telemetry/stream_instrumentation.rs:247), even with `store_raw_bodies: true`. Raw SSE payloads, headers, and per-attempt outgoing bodies are not captured. The schema has no raw-header store. `ResponseHeaders` is a declared event that adapters do not emit.

**Needed:** preserve original inbound JSON, the exact outbound body per attempt, raw response/events and relevant headers alongside normalized data. Preserve returned provider model identity rather than substituting only the configured candidate name.

### A06 — P1: the fine-grained timing model is incomplete and partly mislabeled

**Partial/incorrect; source verified.** [writer.rs:362](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:362) discards `FirstProviderByte` and `FirstGenerationChunk`; the former has no producer. `Connected` is timestamped after `reqwest.send().await` returns response headers, so [connection_latency_us](S:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs:387) includes upstream time to headers rather than measuring TCP/TLS establishment.

Non-streaming outputs are decoded in full before synthetic content events are created. Their timestamps cannot represent first token or inter-chunk generation timing. Reasoning/tool-only output does not trigger `FirstGenerationChunk`. Chunk byte counts measure extracted delta strings, not raw wire bytes. There are no distinct normalized/last-chunk/provider-close/client-close observations. Durations use wall-clock subtraction rather than a monotonic clock.

Some streamed timings are derivable from retained chunk timestamps, but A02 can remove those rows. The application contains no actual TTFT/TPS aggregation API. **Needed:** define timing semantics explicitly, preserve meaningful lifecycle timestamps, distinguish measured/derived/unavailable values, and use monotonic durations.

### A07 — P1: reasoning controls and reasoning visibility are not connected end to end

**Partial/incorrect; discarded controls reproduced.** `GatewayRequest.reasoning` participates in capability filtering but [build_chat_body](S:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs:111) does not send it to providers. This includes enabled/disabled state, effort, and budget. An explicit provider-specific option can work around some cases; the canonical control itself has no execution effect.

The parser captures string `reasoning_content`/`reasoning` and reasoning token counts. It does not normalize summaries, encrypted content, structured reasoning details, or provider metadata. `ReasoningOutput` exists but is unused in execution. [writer.rs:524](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:524) records visibility as `none` whenever reasoning text is absent, even when reasoning tokens were reported. This directly contradicts the conversation's distinction between no exposed text and no reasoning.

**Needed:** adapter mappings for controls and a used reasoning representation that preserves exposed, summary-only, count-only and unavailable states. Hidden provider-internal reasoning is explicitly outside the capture promise.

### A08 — P1: Chat Completions loses usage and additional choices

**Partial/incorrect; source verified.** There is no `stream_options` field or automatic request for streamed usage. [event_to_sse](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:431) discards `UsageUpdate` even when the provider sends it. Non-streaming client responses keep only three token totals, dropping detailed cached/reasoning fields that may have reached telemetry.

`n` is forwarded, but the adapter reads only [choices[0]](S:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs:279), including in streaming parsing. The remaining paid-for completions disappear from the client and corpus. Missing usage fields become zero, conflating unavailable measurements with measured zero; a JSON `usage: null` is treated as a usage update. Multiple cumulative updates create multiple database rows with no final/delta distinction, which can inflate naive analytics.

Additional compatibility gaps: string-valued `stop` cannot deserialize; `developer` role is absent; `max_completion_tokens`, top-level `reasoning_effort`, `parallel_tool_calls` and other unknown fields are silently lost. `modalities` is accepted but never forwarded. Refusal/logprob/audio/provider extension data is not preserved. This is a useful subset, not a full drop-in API.

**Needed:** either implement these contracts or reject unsupported fields explicitly, preserve every choice, request/serialize usage, and retain unknown-vs-zero semantics.

### A09 — P1: Responses is only a text-only shim

**Partial/incorrect; input loss and forced non-streaming reproduced.** [responses.rs:58](S:/Code/RustProjects/AI-Gateway/src/server/routes/responses.rs:58) accepts a string or an array of strings but discards standard object-form message/input items. It sets [stream: false](S:/Code/RustProjects/AI-Gateway/src/server/routes/responses.rs:92), regardless of the caller. Tools, structured output, reasoning, provider options, routing hints and metadata are reset to absent. Tools/reasoning from the provider are not serialized into response output. Usage is returned with Chat Completions names rather than Responses input/output-token names.

No native upstream Responses execution or stateful continuation is implemented; all execution goes through chat completions. There is also no `RouteSelected` emission in this handler. Official [Responses streaming documentation](https://platform.openai.com/docs/api-reference/responses-streaming?lang=python) confirms that `stream: true` requests SSE.

**Needed:** proper input normalization and Responses serialization/events, or an explicitly documented and validated restricted subset that rejects unsupported requests instead of silently changing them.

### A10 — P1: some advertised provider integrations cannot work as configured

**Gemini URL defect reproduced; remaining provider coverage source verified.** The generic endpoint builder always appends `/v1/...` unless the base already ends in `/v1`. With the supplied Gemini base, it produces `/v1beta/openai/v1/chat/completions`. The documented endpoint is `/v1beta/openai/chat/completions`. The same defect affects model listing/health and embeddings. See [endpoint()](S:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs:73), [config](S:/Code/RustProjects/AI-Gateway/config/gateway.yaml:49), and [Google's official reference](https://ai.google.dev/gemini-api/docs/openai).

The Hugging Face example points at `api-inference.huggingface.co`; current official chat routing uses `https://router.huggingface.co/v1`. See [Hugging Face's reference](https://huggingface.co/docs/inference-providers/index). Live responses from the old endpoint were not tested.

All named adapters ultimately use the same OpenAI-compatible implementation. Gemini native fallback, Ollama native fallback, llama.cpp GPU/slot extraction, and OpenRouter upstream-provider extraction are comments/TODOs rather than implemented paths. `upstream_provider` is always `None`. Hardcoded provider-wide capability declarations do not establish support for each model/API operation. Google's documented thinking options also use a nested Google-specific body; the repository's direct top-level `thinkingConfig` example is not translated into that contract.

**Needed:** provider-aware endpoint joining, correct examples, fixture/live contract tests per supported operation, and preservation of provider-specific observations.

### A11 — P1: constraints are bypassed or never enforced

**Disabled-provider execution reproduced; remaining gaps source verified.** Both explicit-provider and explicit-model branches [return before filtering](S:/Code/RustProjects/AI-Gateway/src/routing/mod.rs:108). Chat/Responses then fetch the adapter directly, so `enabled: false` does not prevent execution. An explicit override is reasonable for selection policy, but should not silently disable administrative availability constraints.

Alias capability checks use candidate/alias flags only, not discovered provider capabilities. There is no context-size estimation or enforcement of `max_context_length`/`context_window`, no streaming-capability check, no `routing.candidates` override, and no enforcement of `max_concurrent`. With `routing.provider`, an alias or already-qualified model string is passed verbatim rather than resolved to that provider's deployment.

**Needed:** separate explicit selection from validation; enforce capability/context/concurrency contracts or reject unsupported configuration. Record specific rejection reasons rather than a generic missing-capability label.

### A12 — P1: fallback policy and attempt timeouts are inconsistent

**Unauthorized-by-policy fallback reproduced; other paths source verified.** [chat_completions.rs:354](S:/Code/RustProjects/AI-Gateway/src/server/routes/chat_completions.rs:354) returns an error only when both `should_fallback` and `retryable` are false. After retries are exhausted, a retryable 503 still falls through to another provider even with `fallback_on: []`. Generic execution errors also unconditionally continue to another provider. The same structure exists in Responses.

The outer timeout wraps only `provider.execute()`. For streaming, that ends at response headers; stream consumption is outside `attempt_timeout_secs`. Reqwest's provider-level timeout still applies, so there is a different timeout, not necessarily an unbounded network request. Retry delay fields in `RetryConfig` are unused and the hand-coded backoff is uncapped. The configuration name says retries, while the loop treats the number as total attempts, with zero clamped to one.

**Needed:** one shared execution policy with separate retry/fallback decisions, well-defined full-attempt/idle timeouts, consistent normalized errors, and tests through handlers rather than just the predicate function.

### A13 — P2: operational latency and request metrics do not mean what they claim

**Incorrect; source verified.** [main.rs:136](S:/Code/RustProjects/AI-Gateway/src/main.rs:136) records health-check `/models` latency into the provider request-latency average. Chat and Responses never call `record_latency`; embeddings do. Thus `lowest_latency` mostly routes using health-probe timings, and `/metrics.avg_latency_ms` is not inference latency. It is a lifetime arithmetic mean, not the documented rolling/EMA statistic. Priority still dominates latency.

`/metrics.gateway.total_requests` sums attempts, not incoming requests, and excludes routing failures. Responses [ends the active request before consuming its stream](S:/Code/RustProjects/AI-Gateway/src/server/routes/responses.rs:171), and can decrement it again on stream error, affecting concurrent request counts. Health probes are sequential and can spend provider-sized timeouts on disabled/unreachable providers. `/health` does not assess database persistence.

**Needed:** distinct gateway-request/provider-attempt/health-probe measures; inference latency observations; lifecycle-balanced counters; independent telemetry readiness; named and tested policy semantics.

### A14 — P2: embeddings bypass the shared execution and telemetry pipeline

**Partial; source verified.** [embeddings.rs:69](S:/Code/RustProjects/AI-Gateway/src/server/routes/embeddings.rs:69) resolves candidates directly and tries them without the configured routing policy, retry rules or gateway attempt timeout. It stores only request and completion events: no attempt records, returned vectors/raw response, usage, or cost. Skipped candidates still increment the reported attempt count.

The provider method accepts only model/input, so `dimensions`, `encoding_format`, metadata and provider-specific options are not propagated. It checks provider-wide embedding capability rather than deployment capability. **Needed:** embeddings as a first-class operation in the common execution/observation lifecycle.

### A15 — P1: identity and provenance are insufficient for a permanent corpus

**Partial/incorrect; source verified.** Client-supplied UUID request IDs are used as database primary keys. [RequestReceived upserts](S:/Code/RustProjects/AI-Gateway/src/telemetry/writer.rs:231) rewrite existing request metadata; later rows append using the same request/attempt numbers. Reusing a correlation ID can merge separate inferences and update multiple attempt rows. Non-UUID request headers are propagated by middleware but handlers generate a different internal UUID.

No provider endpoint, immutable deployment/config snapshot reference, model revision/quantization, or actual returned-model identity is persisted per attempt. `model_snapshots` and `provider_snapshots` exist only as tables; nothing writes them. There is no conversation/session/turn relationship or parent response relationship. Complete messages provide useful context snapshots, but not explicit conversation lineage. Tool result messages remain in input JSON and are not linked into normalized tool result records.

**Needed:** a gateway-owned unique inference ID separate from client correlation/idempotency keys; unique attempt keys; versioned configuration/model/provider provenance; and conversation/tool lineage appropriate for later datasets.

### A16 — P2: analysis, presentation, visualization and curated datasets are missing

**Missing, with timing not specified in the conversation.** The repository has no Next.js/TypeScript application, Python analysis package, historical query API, dashboard, charts, conversation explorer, request timeline viewer, filters/search/export workflow, or dataset builder. `/metrics` is only live in-memory JSON. Documentation contains sample SQL; that is not an implemented analysis layer.

No SFT/DPO/evaluation export pipeline, annotation/accept/reject records, preference pairs, quality signals, dataset versions, deduplication or derived-content analysis exists. These later corpus uses were explicitly described, and the user emphasized not neglecting presentation/analysis. **Needed:** a scoped implementation plan after reliable capture, starting with request/attempt/timeline exploration and trustworthy metric definitions, then curation/export.

### A17 — P2: documentation describes capabilities that do not exist

**Incorrect; source verified.** Examples include:

- README claims full drop-in API compatibility and cost tracking, while A08/A09 and the absent cost producer contradict it.
- [Telemetry documentation](S:/Code/RustProjects/AI-Gateway/docs/TELEMETRY_AND_DATABASE.md:38) shows transactional batches; the writer executes statements directly on the pool.
- Its schema diagram/table lists `client_disconnects`, `evaluations`, and `dataset_entries`, which the migration does not create. Several documented cost/error/tool columns do not match actual names/types.
- [Routing documentation](S:/Code/RustProjects/AI-Gateway/docs/ROUTING_AND_POLICIES.md:118) claims context enforcement and EMA latency selection; neither exists as described.
- README and routing YAML snippets omit required `ModelDefinition.name`, so copying them fails deserialization.
- Architecture documentation says all entities use UUIDv7, but most database row IDs are BIGSERIAL; it says responses flow through `GatewayResponse`, which is unused.
- API documentation shows detailed usage and initial assistant-role SSE chunks that serializers do not emit, plus authentication/validation behaviors not implemented.
- The README's Rust 1.85 minimum does not account for let-chain syntax, stabilized in [Rust 1.88](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0/); this audit used Rust 1.89.0. No supported minimum-toolchain check exists.

**Needed:** align docs and runnable examples with tested behavior, and clearly mark scaffolds/future work. The README's 21-test count is also stale; the pre-audit suite has 27.

### A18 — P2: configuration and escape hatches can silently change the contract

**Incorrect/partial; source verified.** Unknown policy strings (including several enum variants such as `cheapest`, `local_first`, `fastest`, `balanced`) silently execute ordered routing while recording the supplied name. Unknown fallback strings are silently dropped. Configuration has no semantic validation; zero batch size panics when creating the channel, and zero flush interval panics in the writer task. Required provider names also prevent the intended missing-name fallback in `main` from being reached.

[build_chat_body](S:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs:172) merges matching option namespaces in `HashMap` iteration order, so conflicting generic/provider-specific keys have no stable precedence. Options may overwrite `model` or `stream` after routing; telemetry still reports the original candidate, and parsing follows the original stream flag. That undermines reproducible routing and can produce protocol mismatch.

`.env` is present and untracked, while `.gitignore` contains only `/target`; there is no dotenv loader. A `.env` file alone does not configure database/provider keys for this binary. Its contents were not inspected. **Needed:** configuration validation, explicit supported policies, deterministic option precedence, validation of protocol-defining overrides, and clear environment loading/ignore behavior.

## Requirement coverage matrix

| Conversation requirement | Status | Evidence / boundary |
|---|---|---|
| Rust gateway using Axum/Tokio/Reqwest/Serde/Tower/SQLx | Implemented | Cargo dependencies and executable startup/server |
| PostgreSQL as primary datastore | Implemented foundation | Migration and writer ran against isolated PostgreSQL; reliability A01/A02 |
| Provider-independent canonical request | Partial | Own type exists, but chat deserializes directly into it; raw protocol boundary absent |
| Canonical response/generation event pipeline | Partial | `ProviderEvent` is used; `GatewayResponse` and `GatewayStreamEvent` are unused scaffolds |
| Provider trait with capabilities/models/health/execute | Implemented | `src/providers/mod.rs`; embeddings has its own method |
| Connection pooling and async stream delivery | Implemented | Shared per-adapter Reqwest clients, lazy SSE body streams |
| Database outside stream's critical path | Implemented | Nonblocking channel; achieved by dropping on pressure, A01 |
| Chat Completions JSON and SSE | Partial/incorrect | Basic single-choice text/tool/reasoning works; A04/A07/A08 |
| Responses API | Partial/incorrect | Text shim; A09 |
| Embeddings API | Partial | Basic forwarding; A14 |
| Model aliases and explicit provider/model IDs | Implemented with defects | Alias priority and slash parsing work; A11/A15 |
| Model/provider registry | Implemented | Static config, runtime adapters and status |
| Model discovery | Partial | `/v1/models` queries providers; metadata discarded, not stored as registry snapshots |
| Capability metadata and filtering | Partial | Four alias/candidate checks; no context/streaming/provider intersection |
| Provider options escape hatch | Implemented with defects | Parameter passthrough/enrichment; A18 |
| Deterministic routing, no AI router | Implemented | Ordered routing and deterministic policy code; no model classifier |
| Ordered fallback and retry | Partial/incorrect | Execution loops exist; policy bypass and missing observations, A03/A12 |
| Health-based filtering | Partial | Alias paths filter; explicit paths bypass; health worker serial |
| Why a route was selected/rejected | Partial | Successful chat routing stored; errors/Responses/embeddings incomplete |
| `/health`, `/providers`, `/metrics` | Implemented with limitations | Registered; metrics/readiness semantics A13 |
| `/models` management path | Missing literal route | `/v1/models` exists; no `/models` alias from the proposed v1 list |
| Full request/response/thinking capture | Partial/incorrect | A03–A08, A14 |
| Every provider attempt preserved | Incorrect | A03 |
| Raw JSON/usage/headers alongside normalized data | Partial | Raw usage captured when present; raw request fidelity/response/headers missing |
| Fine-grained stream timeline | Partial/incorrect | Chunk metadata exists; loss/timestamp gaps A02/A06 |
| Durable ingestion/outbox | Missing/deferred | Explicitly later in conversation; current queue has no replay |
| Safe persistence during database outage/shutdown | Incorrect | A01 |
| Normalized reasoning visibility and summaries | Scaffold only / incorrect | Types/schema exist; runtime writes exposed/none only |
| Model/provider snapshots and provenance | Scaffold only | Tables without writers; A15 |
| Cost accounting | Scaffold only | Cost event/table/writer branch without any producer or pricing model |
| Local hardware/slot/queue observations | Missing | No producer, polling collector, or schema fields |
| Historical analytics API | Missing | No database read/query service |
| Dashboard/data presentation/visualization | Missing | No UI implementation |
| Python analysis / reusable corpus tooling | Missing/deferred | No Python package/workflow |
| Dataset candidates, annotations, accepted/rejected pairs | Missing/deferred | No schema/workflow |
| SFT/DPO/evaluation sets | Missing/deferred | Only illustrative SQL; no tested export or curation |
| OTLP export | Missing | `tracing` logging exists; no OTLP dependencies/exporter |
| Cost/local-first/quality/balanced routing | Missing/deferred | Enum/string names fall back to ordered; local-first can be manually approximated with priorities |
| Latency routing and load balancing | Partial | Equal-priority round robin exists; latency input incorrect A13 |
| Circuit breakers, health scoring, rate limiting | Missing/deferred | Basic health flags/counters only; `max_concurrent` unused |
| Prompt/semantic caching and evaluation hooks | Missing/deferred | No implementations |
| Reranking, image generation, TTS/ASR/audio/video | Missing/deferred | Proposed future breadth, not a v1 failure; image URL input exists |
| Learned/AI model routing | Intentionally absent | Matches explicit deterministic-only instruction |
| Multi-user auth, elaborate billing, Kubernetes/distributed workers | Intentionally outside initial scope | No implementation; not counted as missing v1 requirements |

## Provider coverage

| Provider named in conversation | Actual implementation |
|---|---|
| llama-server | Generic OpenAI-compatible chat/models/embeddings plus sampling-option aliases; no local metrics or native Responses |
| OpenRouter | Generic chat/models with headers/options; no upstream identity/cost extraction; embeddings disabled in adapter |
| Ollama | Compatibility API only; native fallback/model management TODOs |
| Ollama Cloud | Separate wrapper, same compatibility protocol; no cloud-specific contract validation |
| vLLM | Compatibility API plus option aliases; no native Responses or model-specific capability discovery |
| Google AI, non-Vertex | Gemini wrapper exists; provided base URL breaks endpoint construction; no native fallback |
| Hugging Face API | Compatibility wrapper; outdated example base; no task-specific native inference support |
| Transformers | No direct adapter/runtime; can only be reached if separately exposed through a compatible server |
| xAI, Venice, Featherless | Enum variants instantiate generic fallback adapter; no provider-specific behavior/tests |
| Other OpenAI-compatible providers | Configurable generic adapter; capabilities default to false and operational compatibility is not certified |

## Requested telemetry fields: actual coverage

| Requested observation | Current status |
|---|---|
| Request ID, timestamp | Captured for valid handled requests; ID reuse/invalid-header discrepancy A15 |
| Application, task, tags | Chat only; Responses/embeddings use fixed task/tags and discard supplied metadata |
| Arbitrary labels | Survive chat's normalized raw JSON when enabled; no normalized/queryable label records |
| Requested model | Captured |
| Resolved model, provider | Captured on some completed paths; configured candidate rather than verified returned model |
| Provider endpoint | Not persisted |
| Upstream provider behind OpenRouter | Field exists; always None |
| Input/output/total tokens | Parsed when provider supplies usage; missing values become zero |
| Cached input/reasoning tokens | Parsed into usage telemetry; detailed values omitted from client responses |
| TTFT / TTFB | Incomplete; explicit first-event persistence absent; streamed TTFT derivation depends on retained chunks |
| Generation time | Attempt duration exists; distinct decode/generation phase not reliably measured |
| Total latency | Captured on completed paths, missing on several early returns |
| Tokens per second | No implemented analytics; sample SQL divides output by total attempt duration, which includes prefill/network |
| Inter-chunk latency | Derivable only for retained streamed delta timestamps; non-streaming synthetic events cannot supply it |
| HTTP status | Not persisted in the outcome pipeline; `errors.status_code` is never populated |
| Retry/fallback counts | Total attempts exists, but attempt loss prevents reliable distinction or reconstruction |
| Error type/message | Some stream failures only; HTTP/connection/timeout attempt observations missing |
| Estimated cost | No producer/pricing engine |
| Actual reported cost | May survive inside raw usage if provider supplies it there; never normalized into cost_records |
| Context utilization | No input token/context ratio, validated model limit, or context enforcement |
| Streaming/tools/structured-output/vision flags | Chat flags exist; several are presence checks; other endpoints hardcode false |
| Reasoning visibility, summaries, encrypted output | Incomplete; no runtime use of rich reasoning type |
| GPU memory, slot, queue duration | Not collected |
| Full generation text/tool calls | Successful first-choice generations only; interrupted/failed content lost |
| Full raw response and raw headers | Not collected |
| Conversation/dataset lineage | Not explicitly modeled |

## Remediation order

1. **Protect observations first:** fix batch dependency order, reliable ingestion/retry/shutdown, independent inference IDs, and capture original/provider-native payloads. Add database-backed failure/recovery tests.
2. **Unify execution lifecycle:** record every attempt and terminal outcome, fix streaming failure/cancellation, preserve partial outputs, and apply retry/fallback/timeouts consistently across endpoints.
3. **Make protocol/provider support truthful:** fix Gemini/Hugging Face configuration, implement or explicitly constrain Responses, forward supported controls, preserve choices/usage/reasoning and reject unsupported options.
4. **Make telemetry analytically trustworthy:** correct timestamps, unknown-vs-zero values, provider/model snapshots, upstream identity, costs, local metrics, and routing constraints. Define and test metric denominators.
5. **Build the requested exploration layer:** historical query API, request/attempt/timeline UI, filters and comparisons, then annotations, versioned exports and dataset workflows.
6. **Add explicitly deferred operations as needed:** OTLP, journal expansion if not used in step 1, circuit breakers, caching, further policies/modalities. Update documentation and contract tests with each delivered capability.

The current project is suitable as a foundation for continued development. It should not yet be treated as the sole permanent archive of inference activity or as a fully compatible replacement for every advertised endpoint.
