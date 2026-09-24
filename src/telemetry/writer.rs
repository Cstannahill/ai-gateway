use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Postgres, QueryBuilder, Transaction};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time;

use crate::config::PricingEntry;

use super::events::{AttemptStatus, TelemetryEvent};
use super::health::{TelemetryHealth, TelemetryMode};

// ─── Telemetry Writer ────────────────────────────────────────────────────────
//
// Background task that batches telemetry events and writes them to PostgreSQL.
//
// NOT in the stream's critical path:
//   Provider chunk → client (immediate)
//                  → bounded channel → writer → batch → Postgres (async)
//
// Each flush runs as a single transaction (see flush_batch): events that
// create/update a parent `inference_requests` row are applied before the
// bulk `stream_events` insert, since `stream_events.request_id` has an
// immediate foreign key — reversing that order silently dropped chunks for
// any request whose RequestReceived event landed in the same flush window
// (docs/REMEDIATION_PLAN.md Phase 0.1 / Audit A02). A failed flush is
// retried a bounded number of times before falling back to an append-only
// JSONL log rather than being discarded (Phase 0.3); that log is itself
// replayed automatically on writer startup and on every subsequent flush
// tick, so an extended outage drains itself once Postgres recovers,
// without operator intervention (see `replay_fallback_log`, Phase 5.1).

const MAX_FLUSH_ATTEMPTS: u32 = 3;

pub struct TelemetryWriter {
    pool: Option<sqlx::PgPool>,
    batch_size: usize,
    flush_interval: Duration,
    store_raw_bodies: bool,
    store_stream_events: bool,
    fallback_log_path: PathBuf,
    /// Deployment pricing, keyed by `"provider/model"`. See
    /// `GatewayConfig::pricing`'s doc comment.
    pricing: HashMap<String, PricingEntry>,
    /// Shared, live health this writer updates on every flush/replay
    /// outcome — see `docs/frontend/API_GAPS.md` item 8. Created fresh by
    /// this writer (not passed in), exposed via `health()` for whoever
    /// constructed the writer (`TelemetryPipeline`) to hand to `AppState`.
    health: Arc<TelemetryHealth>,
}

impl TelemetryWriter {
    pub async fn new(
        database_url: &str,
        pool_size: u32,
        batch_size: usize,
        flush_interval_ms: u64,
        store_raw_bodies: bool,
        store_stream_events: bool,
    ) -> anyhow::Result<Self> {
        Self::new_with_fallback_log(
            database_url,
            pool_size,
            batch_size,
            flush_interval_ms,
            store_raw_bodies,
            store_stream_events,
            "telemetry_fallback.jsonl",
            HashMap::new(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_fallback_log(
        database_url: &str,
        pool_size: u32,
        batch_size: usize,
        flush_interval_ms: u64,
        store_raw_bodies: bool,
        store_stream_events: bool,
        fallback_log_path: impl Into<PathBuf>,
        pricing: HashMap<String, PricingEntry>,
    ) -> anyhow::Result<Self> {
        let pool = match PgPoolOptions::new()
            .max_connections(pool_size)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await
        {
            Ok(p) => {
                tracing::info!("Connected to PostgreSQL telemetry database");
                // Run migrations automatically
                if let Err(e) = sqlx::migrate!("./migrations").run(&p).await {
                    tracing::warn!(error = %e, "Failed to apply database migrations");
                } else {
                    tracing::info!("Database migrations applied successfully");
                }
                Some(p)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Could not connect to PostgreSQL. Telemetry will run in log-only mode."
                );
                None
            }
        };

        let mode = if pool.is_some() { TelemetryMode::Database } else { TelemetryMode::Unavailable };

        let writer = Self {
            pool,
            batch_size,
            flush_interval: Duration::from_millis(flush_interval_ms),
            store_raw_bodies,
            store_stream_events,
            fallback_log_path: fallback_log_path.into(),
            pricing,
            health: Arc::new(TelemetryHealth::new(mode)),
        };

        // A prior run (this process restarting, or an entirely separate
        // one against the same fallback log path) may have left events
        // behind from an outage that has since ended — replay them now
        // rather than requiring an operator to notice and intervene
        // manually (docs/REMEDIATION_PLAN.md Phase 5.1).
        writer.replay_fallback_log().await;

        Ok(writer)
    }

    /// Get a handle to this writer's shared, live health — see
    /// `docs/frontend/API_GAPS.md` item 8.
    pub fn health(&self) -> Arc<TelemetryHealth> {
        self.health.clone()
    }

    /// Run the writer loop — receives events and batches them for persistence.
    pub async fn run(self, mut receiver: mpsc::Receiver<TelemetryEvent>) {
        let mut batch: Vec<TelemetryEvent> = Vec::with_capacity(self.batch_size);
        let mut flush_interval = time::interval(self.flush_interval);

        tracing::info!(
            batch_size = self.batch_size,
            flush_interval_ms = self.flush_interval.as_millis() as u64,
            has_db = self.pool.is_some(),
            "Telemetry writer started"
        );

        loop {
            tokio::select! {
                // Receive events from the channel.
                event = receiver.recv() => {
                    match event {
                        Some(evt) => {
                            tracing::trace!(
                                event_type = %evt.event_type(),
                                request_id = %evt.request_id(),
                                "Telemetry event received"
                            );

                            batch.push(evt);

                            if batch.len() >= self.batch_size {
                                self.flush_batch(&mut batch).await;
                            }
                        }
                        None => {
                            // Channel closed — flush remaining events and exit.
                            if !batch.is_empty() {
                                self.flush_batch(&mut batch).await;
                            }
                            tracing::info!("Telemetry writer shutting down");
                            return;
                        }
                    }
                }
                // Periodic flush for low-throughput periods. Also the
                // heartbeat for retrying any backlog left in the fallback
                // log by an earlier outage — an extended Postgres outage
                // that later recovers gets drained automatically on this
                // same cadence, without needing a process restart
                // (docs/REMEDIATION_PLAN.md Phase 5.1).
                _ = flush_interval.tick() => {
                    self.replay_fallback_log().await;
                    if !batch.is_empty() {
                        self.flush_batch(&mut batch).await;
                    }
                }
            }
        }
    }

    /// Flush a batch of events to PostgreSQL.
    ///
    /// Runs the whole batch as one transaction, retries on failure, and
    /// only as a last resort appends the batch to a local JSONL file rather
    /// than discarding it (see module docs and Phase 0.2/0.3).
    async fn flush_batch(&self, batch: &mut Vec<TelemetryEvent>) {
        let count = batch.len();
        tracing::debug!(count, "Flushing telemetry batch");

        let Some(ref pool) = self.pool else {
            // Log-only fallback (no database configured).
            for event in batch.iter() {
                tracing::debug!(
                    event_type = %event.event_type(),
                    request_id = %event.request_id(),
                    "Persisting telemetry event (log-only)"
                );
            }
            batch.clear();
            return;
        };

        let mut last_error = None;
        for attempt in 0..MAX_FLUSH_ATTEMPTS {
            match self.try_flush_batch_transactional(pool, batch).await {
                Ok(()) => {
                    batch.clear();
                    tracing::debug!(count, "Telemetry batch flushed");
                    self.health.record_flush_success(chrono::Utc::now());
                    return;
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        attempt = attempt + 1,
                        max_attempts = MAX_FLUSH_ATTEMPTS,
                        "Failed to flush telemetry batch"
                    );
                    last_error = Some(e);
                    if attempt + 1 < MAX_FLUSH_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                    }
                }
            }
        }

        tracing::error!(
            count,
            error = ?last_error,
            path = %self.fallback_log_path.display(),
            "Telemetry batch failed after all retries; writing to fallback log instead of discarding"
        );
        self.health.record_flush_failure();
        if let Err(e) = self.write_fallback_log(batch).await {
            tracing::error!(
                error = %e,
                count,
                "Failed to write telemetry fallback log — these events are lost"
            );
        }
        batch.clear();
    }

    /// Attempt to persist an entire batch inside a single transaction.
    ///
    /// Order matters: events that create/update the parent `inference_requests`
    /// row (everything except `StreamChunk`) are applied first, in their
    /// original relative order, and the `StreamChunk` bulk insert runs last —
    /// so a request whose `RequestReceived` and chunk events land in the same
    /// flush window always has its parent row in place before the FK-checked
    /// bulk insert runs (Audit A02 / CL05).
    async fn try_flush_batch_transactional(
        &self,
        pool: &sqlx::PgPool,
        batch: &[TelemetryEvent],
    ) -> anyhow::Result<()> {
        let mut tx = pool.begin().await?;

        for event in batch.iter() {
            if matches!(event, TelemetryEvent::StreamChunk { .. }) {
                continue;
            }
            self.persist_event(&mut tx, event).await?;
        }

        if self.store_stream_events {
            let stream_chunks: Vec<_> = batch
                .iter()
                .filter_map(|e| {
                    if let TelemetryEvent::StreamChunk {
                        request_id,
                        attempt_number,
                        timestamp,
                        sequence,
                        chunk_type,
                        byte_count,
                        synthetic,
                    } = e
                    {
                        Some((
                            *request_id,
                            *attempt_number as i32,
                            *timestamp,
                            *sequence as i64,
                            chunk_type.as_str(),
                            *byte_count as i32,
                            *synthetic,
                        ))
                    } else {
                        None
                    }
                })
                .collect();

            if !stream_chunks.is_empty() {
                let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
                    "INSERT INTO stream_events (request_id, attempt_number, timestamp, sequence, chunk_type, byte_count, synthetic) ",
                );
                builder.push_values(stream_chunks, |mut b, (req_id, attempt, ts, seq, c_type, bytes, synthetic)| {
                    b.push_bind(req_id)
                        .push_bind(attempt)
                        .push_bind(ts)
                        .push_bind(seq)
                        .push_bind(c_type)
                        .push_bind(bytes)
                        .push_bind(synthetic);
                });
                builder.build().execute(&mut *tx).await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    /// Durability backstop: append the whole failed batch to a local JSONL
    /// file, one event per line, instead of losing it silently. Paired
    /// with `replay_fallback_log` (called on construction and on every
    /// periodic flush tick), this makes "Postgres was down" degrade to
    /// "these events are delayed" rather than requiring manual recovery or
    /// risking loss on process restart (docs/REMEDIATION_PLAN.md Phase
    /// 5.1). Not a full write-ahead-logged outbox — events are journaled
    /// only after the in-memory batch already failed its DB attempts, not
    /// before every write — so a hard process crash between receiving an
    /// event and a failed flush can still lose it. See the matching note
    /// in docs/ARCHITECTURE.md's Step 4.
    async fn write_fallback_log(&self, batch: &[TelemetryEvent]) -> anyhow::Result<()> {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.fallback_log_path)
            .await?;

        let mut buf = String::new();
        for event in batch {
            buf.push_str(&serde_json::to_string(event)?);
            buf.push('\n');
        }
        file.write_all(buf.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    /// Replay any events left in the fallback log by an earlier outage.
    /// Reads the whole file, parses each line as a `TelemetryEvent`
    /// (skipping and warning on any line that fails to parse — e.g. a
    /// partially written line from a crash mid-append — rather than
    /// blocking replay of everything else), and flushes them through the
    /// normal transactional path in `batch_size`-sized chunks. Only
    /// removes the file once every parsed event has been persisted; on a
    /// failure partway through, rewrites the file with just what's left so
    /// a later attempt (the next tick, or the next process start) resumes
    /// instead of reprocessing already-persisted events. A no-op if there
    /// is no database configured or no file exists.
    async fn replay_fallback_log(&self) {
        let Some(ref pool) = self.pool else { return };

        let contents = match tokio::fs::read_to_string(&self.fallback_log_path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %self.fallback_log_path.display(),
                    "Could not read telemetry fallback log for replay"
                );
                return;
            }
        };

        if contents.trim().is_empty() {
            return;
        }

        // The file exists and has real content — something is genuinely
        // pending, regardless of how the replay attempt below turns out.
        self.health.set_fallback_pending(true);

        let mut events = Vec::new();
        let mut malformed = 0u32;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TelemetryEvent>(line) {
                Ok(event) => events.push(event),
                Err(_) => malformed += 1,
            }
        }
        if malformed > 0 {
            tracing::warn!(
                malformed,
                path = %self.fallback_log_path.display(),
                "Skipped malformed lines while replaying telemetry fallback log"
            );
        }
        if events.is_empty() {
            // Nothing parseable survived (e.g. the file was only ever
            // malformed lines) — clear it so this doesn't spin forever.
            let _ = tokio::fs::remove_file(&self.fallback_log_path).await;
            self.health.set_fallback_pending(false);
            return;
        }

        tracing::info!(count = events.len(), "Replaying telemetry fallback log");

        let mut remaining = events;
        while !remaining.is_empty() {
            let chunk_len = remaining.len().min(self.batch_size.max(1));
            let chunk: Vec<TelemetryEvent> = remaining.drain(..chunk_len).collect();
            if let Err(e) = self.try_flush_batch_transactional(pool, &chunk).await {
                tracing::warn!(
                    error = %e,
                    remaining = chunk.len() + remaining.len(),
                    "Telemetry fallback log replay stalled; will retry on the next tick"
                );
                // Put the failed chunk back in front of whatever else was
                // still pending, then stop — the DB is presumably still
                // unhappy, so further chunks would likely fail too.
                remaining.splice(0..0, chunk);
                break;
            }
        }

        if remaining.is_empty() {
            if let Err(e) = tokio::fs::remove_file(&self.fallback_log_path).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(error = %e, "Replayed telemetry fallback log but could not remove it");
            }
            tracing::info!("Telemetry fallback log fully replayed and cleared");
            self.health.set_fallback_pending(false);
        } else {
            let mut buf = String::new();
            for event in &remaining {
                if let Ok(s) = serde_json::to_string(event) {
                    buf.push_str(&s);
                    buf.push('\n');
                }
            }
            if let Err(e) = tokio::fs::write(&self.fallback_log_path, buf).await {
                tracing::error!(
                    error = %e,
                    "Failed to rewrite telemetry fallback log after partial replay — \
                     already-persisted events may be replayed again on the next attempt"
                );
            }
            self.health.set_fallback_pending(true);
        }
    }

    /// Persist a single event into the appropriate relational table, as
    /// part of the caller's transaction.
    async fn persist_event(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &TelemetryEvent,
    ) -> anyhow::Result<()> {
        match event {
            TelemetryEvent::RequestReceived {
                request_id,
                timestamp,
                client_correlation_id,
                application,
                task,
                tags,
                requested_model,
                streaming,
                canonical_messages,
                raw_request,
                uses_tools,
                uses_vision,
                uses_structured_output,
                uses_reasoning,
            } => {
                let raw_req = if self.store_raw_bodies {
                    raw_request.as_ref()
                } else {
                    None
                };

                // `request_id` is the gateway-generated primary key and is
                // never overwritten by ON CONFLICT — a client reusing the
                // same `client_correlation_id` (e.g. a stale x-request-id
                // header) across distinct requests must not merge their
                // data. Each request_id is unique per gateway-generated
                // UUIDv7, so this INSERT should never actually conflict in
                // normal operation; ON CONFLICT DO NOTHING here only guards
                // against a duplicate RequestReceived for the same request
                // (e.g. a retried telemetry send), not against ID reuse.
                sqlx::query(
                    r#"
                    INSERT INTO inference_requests (
                        request_id, received_at, client_correlation_id, application, task,
                        requested_model, streaming, uses_tools, uses_vision,
                        uses_structured_output, uses_reasoning, canonical_messages, raw_request
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                    ON CONFLICT (request_id) DO NOTHING
                    "#,
                )
                .bind(request_id)
                .bind(timestamp)
                .bind(client_correlation_id)
                .bind(application)
                .bind(task)
                .bind(requested_model)
                .bind(streaming)
                .bind(uses_tools)
                .bind(uses_vision)
                .bind(uses_structured_output)
                .bind(uses_reasoning)
                .bind(canonical_messages)
                .bind(raw_req)
                .execute(&mut **tx)
                .await?;

                for tag in tags {
                    sqlx::query(
                        r#"INSERT INTO request_tags (request_id, tag) VALUES ($1, $2)"#,
                    )
                    .bind(request_id)
                    .bind(tag)
                    .execute(&mut **tx)
                    .await?;
                }
            }

            TelemetryEvent::RouteSelected {
                request_id,
                timestamp,
                decision,
            } => {
                // Ensure parent record exists
                sqlx::query(
                    r#"INSERT INTO inference_requests (request_id, requested_model) VALUES ($1, $2) ON CONFLICT (request_id) DO NOTHING"#,
                )
                .bind(request_id)
                .bind(&decision.requested_model)
                .execute(&mut **tx)
                .await?;

                let rejection_reasons = serde_json::to_value(&decision.rejection_reasons).ok();

                sqlx::query(
                    r#"
                    INSERT INTO routing_decisions (
                        request_id, timestamp, requested_model, resolution_type,
                        candidates_considered, candidates_rejected, rejection_reasons,
                        selected_provider, selected_model, policy_applied, short_circuit
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                    "#,
                )
                .bind(request_id)
                .bind(timestamp)
                .bind(&decision.requested_model)
                .bind(&decision.resolution_type)
                .bind(decision.candidates_considered as i32)
                .bind(decision.candidates_rejected as i32)
                .bind(rejection_reasons)
                .bind(&decision.selected_provider)
                .bind(&decision.selected_model)
                .bind(&decision.policy_applied)
                .bind(&decision.short_circuit)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::ProviderAttemptStarted {
                request_id,
                attempt_number,
                timestamp,
                provider,
                model,
            } => {
                sqlx::query(
                    r#"INSERT INTO inference_requests (request_id, requested_model) VALUES ($1, $2) ON CONFLICT (request_id) DO NOTHING"#,
                )
                .bind(request_id)
                .bind(model)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r#"
                    INSERT INTO provider_attempts (
                        request_id, attempt_number, timestamp, provider, model, status
                    ) VALUES ($1, $2, $3, $4, $5, 'started')
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(timestamp)
                .bind(provider)
                .bind(model)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::ProviderConnected {
                request_id,
                attempt_number,
                connection_latency_us,
                ..
            } => {
                sqlx::query(
                    r#"
                    UPDATE provider_attempts
                    SET connection_latency_us = $3
                    WHERE request_id = $1 AND attempt_number = $2
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(*connection_latency_us as i64)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::FirstProviderByte {
                request_id,
                attempt_number,
                timestamp,
                synthetic,
            } => {
                // `synthetic_timing` is sticky (OR'd, never cleared) since
                // either of this attempt's two timing signals being
                // fabricated is enough to disqualify the whole attempt from
                // real TTFB/TTFT analysis (docs/REMEDIATION_PLAN.md Phase
                // 3.1 / Audit A06).
                sqlx::query(
                    r#"
                    UPDATE provider_attempts
                    SET first_byte_at = $3,
                        synthetic_timing = synthetic_timing OR $4
                    WHERE request_id = $1 AND attempt_number = $2 AND first_byte_at IS NULL
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(timestamp)
                .bind(synthetic)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::FirstGenerationChunk {
                request_id,
                attempt_number,
                timestamp,
                synthetic,
                ..
            } => {
                sqlx::query(
                    r#"
                    UPDATE provider_attempts
                    SET first_token_at = $3,
                        synthetic_timing = synthetic_timing OR $4
                    WHERE request_id = $1 AND attempt_number = $2 AND first_token_at IS NULL
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(timestamp)
                .bind(synthetic)
                .execute(&mut **tx)
                .await?;
            }

            // Never reached: try_flush_batch_transactional filters
            // StreamChunk events out before calling persist_event and
            // bulk-inserts them separately (see that function's doc comment
            // for why the ordering matters).
            TelemetryEvent::StreamChunk { .. } => {}

            TelemetryEvent::ProviderAttemptCompleted {
                request_id,
                attempt_number,
                timestamp,
                status,
                finish_reason,
                provider_response_id,
                upstream_provider,
                duration_us,
                error_code,
                error_message,
            } => {
                let status_str = match status {
                    AttemptStatus::Success => "success",
                    AttemptStatus::Failed => "failed",
                    AttemptStatus::Timeout => "timeout",
                    AttemptStatus::RateLimited => "rate_limited",
                    AttemptStatus::Cancelled => "cancelled",
                };

                let res = sqlx::query(
                    r#"
                    UPDATE provider_attempts
                    SET status = $3,
                        finish_reason = $4,
                        provider_response_id = $5,
                        upstream_provider = $6,
                        duration_us = $7,
                        error_code = $8,
                        error_message = $9,
                        finished_at = $10
                    WHERE request_id = $1 AND attempt_number = $2
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(status_str)
                .bind(finish_reason.as_deref())
                .bind(provider_response_id.as_deref())
                .bind(upstream_provider.as_deref())
                .bind(*duration_us as i64)
                .bind(error_code.as_deref())
                .bind(error_message.as_deref())
                .bind(timestamp)
                .execute(&mut **tx)
                .await?;

                // If attempt was not pre-inserted, insert now. `timestamp`
                // serves as both `timestamp` (started_at) and `finished_at`
                // here since, in this edge case, no `ProviderAttemptStarted`
                // was ever observed — this is the only timestamp available.
                if res.rows_affected() == 0 {
                    sqlx::query(
                        r#"
                        INSERT INTO provider_attempts (
                            request_id, attempt_number, timestamp, provider, model, status,
                            finish_reason, provider_response_id, upstream_provider, duration_us,
                            error_code, error_message, finished_at
                        ) VALUES ($1, $2, $3, 'unknown', 'unknown', $4, $5, $6, $7, $8, $9, $10, $3)
                        "#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .bind(timestamp)
                    .bind(status_str)
                    .bind(finish_reason.as_deref())
                    .bind(provider_response_id.as_deref())
                    .bind(upstream_provider.as_deref())
                    .bind(*duration_us as i64)
                    .bind(error_code.as_deref())
                    .bind(error_message.as_deref())
                    .execute(&mut **tx)
                    .await?;
                }

                // If there's an error code, record in errors table
                if let Some(code) = error_code {
                    sqlx::query(
                        r#"
                        INSERT INTO errors (
                            request_id, attempt_number, timestamp, error_code, error_message
                        ) VALUES ($1, $2, $3, $4, $5)
                        "#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .bind(timestamp)
                    .bind(code)
                    .bind(error_message.as_deref())
                    .execute(&mut **tx)
                    .await?;
                }
            }

            TelemetryEvent::UsageRecorded {
                request_id,
                attempt_number,
                timestamp,
                usage,
                source,
                kind,
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO generation_usage (
                        request_id, attempt_number, timestamp, prompt_tokens, completion_tokens,
                        total_tokens, cached_tokens, reasoning_tokens, raw_provider_usage, source, kind
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(timestamp)
                .bind(usage.prompt_tokens as i64)
                .bind(usage.completion_tokens as i64)
                .bind(usage.total_tokens as i64)
                .bind(usage.cached_tokens.map(|v| v as i64))
                .bind(usage.reasoning_tokens.map(|v| v as i64))
                .bind(&usage.raw_provider_usage)
                .bind(source)
                .bind(kind)
                .execute(&mut **tx)
                .await?;

                // Covers the ordering where usage arrives after the
                // generation was already persisted as `none` — see the
                // matching comment in the GenerationContent branch above.
                if usage.reasoning_tokens.is_some_and(|t| t > 0) {
                    sqlx::query(
                        r#"
                        UPDATE generations
                        SET reasoning_visibility = 'count_only'
                        WHERE request_id = $1 AND attempt_number = $2 AND reasoning_visibility = 'none'
                        "#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .execute(&mut **tx)
                    .await?;
                }

                // Compute and persist cost if this attempt's provider/model
                // has a configured price — previously `cost_records` had no
                // producer at all regardless of configuration
                // (docs/REMEDIATION_PLAN.md Phase 3.5 / Audit A15/A17).
                if !self.pricing.is_empty() {
                    let deployment: Option<(String, String)> = sqlx::query_as(
                        r#"SELECT provider, model FROM provider_attempts WHERE request_id = $1 AND attempt_number = $2"#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some((provider, model)) = deployment
                        && let Some(price) = self.pricing.get(&format!("{provider}/{model}"))
                    {
                        let input_cost = (usage.prompt_tokens as f64 / 1000.0) * price.input_cost_per_1k;
                        let output_cost = (usage.completion_tokens as f64 / 1000.0) * price.output_cost_per_1k;

                        sqlx::query(
                            r#"
                            INSERT INTO cost_records (
                                request_id, timestamp, estimated_cost, currency, provider, model,
                                input_cost, output_cost
                            ) VALUES ($1, $2, $3, 'USD', $4, $5, $6, $7)
                            "#,
                        )
                        .bind(request_id)
                        .bind(timestamp)
                        .bind(input_cost + output_cost)
                        .bind(&provider)
                        .bind(&model)
                        .bind(input_cost)
                        .bind(output_cost)
                        .execute(&mut **tx)
                        .await?;
                    }
                }
            }

            TelemetryEvent::GenerationContent {
                request_id,
                attempt_number,
                timestamp,
                choice_index,
                content,
                reasoning_content,
                tool_calls,
                raw_response,
                status,
            } => {
                let raw_resp = if self.store_raw_bodies {
                    raw_response.as_ref()
                } else {
                    None
                };

                // Distinguish "the model didn't reason" from "reasoning
                // happened but no text was exposed" — previously any
                // absent `reasoning_content` was written as `none`
                // regardless of whether reasoning tokens were reported,
                // conflating the two cases the `ReasoningVisibility` type
                // exists specifically to keep apart (Audit A07). If a
                // `UsageRecorded` event for this attempt already landed
                // with reasoning_tokens > 0, mark it `count_only` instead;
                // the reverse ordering (usage arriving after this insert)
                // is handled in the UsageRecorded branch below. Reasoning
                // is never attributed to a non-zero choice index (see
                // `TelemetryEvent::GenerationContent`'s field docs), so
                // there's nothing to look up for those — always `none`.
                let visibility = if reasoning_content.is_some() {
                    "exposed"
                } else if *choice_index != 0 {
                    "none"
                } else {
                    let reasoning_tokens_reported: Option<i64> = sqlx::query_scalar(
                        r#"
                        SELECT reasoning_tokens FROM generation_usage
                        WHERE request_id = $1 AND attempt_number = $2 AND reasoning_tokens > 0
                        LIMIT 1
                        "#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .fetch_optional(&mut **tx)
                    .await?;

                    if reasoning_tokens_reported.is_some() {
                        "count_only"
                    } else {
                        "none"
                    }
                };

                sqlx::query(
                    r#"
                    INSERT INTO generations (
                        request_id, attempt_number, choice_index, timestamp, content,
                        reasoning_content, reasoning_visibility, tool_calls, raw_response, status
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(*choice_index as i32)
                .bind(timestamp)
                .bind(content)
                .bind(reasoning_content.as_deref())
                .bind(visibility)
                .bind(tool_calls)
                .bind(raw_resp)
                .bind(status)
                .execute(&mut **tx)
                .await?;

                // Also populate tool_calls table if structured tool calls exist
                if let Some(arr) = tool_calls.as_ref().and_then(|tc| tc.as_array()) {
                    for tc in arr {
                        let tc_id = tc.get("id").and_then(|v| v.as_str());
                        let fn_name = tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let fn_args = tc.get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .or_else(|| tc.get("function").and_then(|f| f.get("arguments")).map(|_| ""))
                            .unwrap_or("");

                        sqlx::query(
                            r#"
                            INSERT INTO tool_calls (
                                request_id, attempt_number, tool_call_id, function_name, arguments
                            ) VALUES ($1, $2, $3, $4, $5)
                            "#,
                        )
                        .bind(request_id)
                        .bind(*attempt_number as i32)
                        .bind(tc_id)
                        .bind(fn_name)
                        .bind(fn_args)
                        .execute(&mut **tx)
                        .await?;
                    }
                }
            }

            TelemetryEvent::CostRecorded {
                request_id,
                timestamp,
                estimated_cost,
                actual_cost,
                currency,
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO cost_records (
                        request_id, timestamp, estimated_cost, actual_cost, currency
                    ) VALUES ($1, $2, $3, $4, $5)
                    "#,
                )
                .bind(request_id)
                .bind(timestamp)
                .bind(estimated_cost)
                .bind(actual_cost)
                .bind(currency)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::RequestCompleted {
                request_id,
                timestamp,
                total_latency_us,
                total_attempts,
                serving_provider,
                resolved_model,
                success,
            } => {
                sqlx::query(
                    r#"
                    UPDATE inference_requests
                    SET completed_at = $2,
                        total_latency_us = $3,
                        total_attempts = $4,
                        serving_provider = $5,
                        resolved_model = $6,
                        success = $7
                    WHERE request_id = $1
                    "#,
                )
                .bind(request_id)
                .bind(timestamp)
                .bind(*total_latency_us as i64)
                .bind(*total_attempts as i32)
                .bind(serving_provider.as_deref())
                .bind(resolved_model.as_deref())
                .bind(success)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::ModelSnapshot {
                timestamp,
                model_name,
                providers,
                capabilities,
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO model_snapshots (snapshot_at, model_name, providers, capabilities)
                    VALUES ($1, $2, $3, $4)
                    "#,
                )
                .bind(timestamp)
                .bind(model_name)
                .bind(serde_json::to_value(providers).ok())
                .bind(capabilities)
                .execute(&mut **tx)
                .await?;
            }

            TelemetryEvent::ProviderSnapshot {
                timestamp,
                provider_name,
                provider_type,
                health,
                total_requests,
                total_errors,
                avg_latency_ms,
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO provider_snapshots (
                        snapshot_at, provider_name, provider_type, health,
                        total_requests, total_errors, avg_latency_ms
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                )
                .bind(timestamp)
                .bind(provider_name)
                .bind(provider_type)
                .bind(health)
                .bind(*total_requests as i64)
                .bind(*total_errors as i64)
                .bind(avg_latency_ms)
                .execute(&mut **tx)
                .await?;
            }
        }

        Ok(())
    }
}
