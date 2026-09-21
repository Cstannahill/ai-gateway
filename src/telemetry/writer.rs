use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, QueryBuilder};
use tokio::sync::mpsc;
use tokio::time;

use super::events::{AttemptStatus, TelemetryEvent};

// ─── Telemetry Writer ────────────────────────────────────────────────────────
//
// Background task that batches telemetry events and writes them to PostgreSQL.
//
// NOT in the stream's critical path:
//   Provider chunk → client (immediate)
//                  → bounded channel → writer → batch → Postgres (async)

pub struct TelemetryWriter {
    pool: Option<PgPool>,
    batch_size: usize,
    flush_interval: Duration,
    store_raw_bodies: bool,
    store_stream_events: bool,
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

        Ok(Self {
            pool,
            batch_size,
            flush_interval: Duration::from_millis(flush_interval_ms),
            store_raw_bodies,
            store_stream_events,
        })
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
                // Periodic flush for low-throughput periods.
                _ = flush_interval.tick() => {
                    if !batch.is_empty() {
                        self.flush_batch(&mut batch).await;
                    }
                }
            }
        }
    }

    /// Flush a batch of events to PostgreSQL.
    async fn flush_batch(&self, batch: &mut Vec<TelemetryEvent>) {
        let count = batch.len();
        tracing::debug!(count, "Flushing telemetry batch");

        if let Some(ref pool) = self.pool {
            // Bulk insert stream chunks if enabled to eliminate N+1 sequential inserts
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
                        } = e
                        {
                            Some((
                                *request_id,
                                *attempt_number as i32,
                                *timestamp,
                                *sequence as i64,
                                chunk_type.as_str(),
                                *byte_count as i32,
                            ))
                        } else {
                            None
                        }
                    })
                    .collect();

                if !stream_chunks.is_empty() {
                    let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
                        "INSERT INTO stream_events (request_id, attempt_number, timestamp, sequence, chunk_type, byte_count) ",
                    );
                    builder.push_values(stream_chunks, |mut b, (req_id, attempt, ts, seq, c_type, bytes)| {
                        b.push_bind(req_id)
                            .push_bind(attempt)
                            .push_bind(ts)
                            .push_bind(seq)
                            .push_bind(c_type)
                            .push_bind(bytes);
                    });
                    if let Err(e) = builder.build().execute(pool).await {
                        tracing::error!(error = %e, "Failed to bulk persist stream_events");
                    }
                }
            }

            // Persist remaining (lifecycle/usage/content) events
            for event in batch.iter() {
                if matches!(event, TelemetryEvent::StreamChunk { .. }) {
                    continue;
                }
                if let Err(e) = self.persist_event(pool, event).await {
                    tracing::error!(
                        error = %e,
                        event_type = %event.event_type(),
                        request_id = %event.request_id(),
                        "Failed to persist telemetry event to database"
                    );
                }
            }
        } else {
            // Log-only fallback
            for event in batch.iter() {
                tracing::debug!(
                    event_type = %event.event_type(),
                    request_id = %event.request_id(),
                    "Persisting telemetry event (log-only)"
                );
            }
        }

        batch.clear();
        tracing::debug!(count, "Telemetry batch flushed");
    }

    /// Persist a single event into the appropriate relational table.
    async fn persist_event(&self, pool: &PgPool, event: &TelemetryEvent) -> anyhow::Result<()> {
        match event {
            TelemetryEvent::RequestReceived {
                request_id,
                timestamp,
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

                sqlx::query(
                    r#"
                    INSERT INTO inference_requests (
                        request_id, received_at, application, task, requested_model,
                        streaming, uses_tools, uses_vision, uses_structured_output, uses_reasoning,
                        canonical_messages, raw_request
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                    ON CONFLICT (request_id) DO UPDATE SET
                        received_at = EXCLUDED.received_at,
                        application = COALESCE(EXCLUDED.application, inference_requests.application),
                        task = COALESCE(EXCLUDED.task, inference_requests.task),
                        requested_model = EXCLUDED.requested_model,
                        streaming = EXCLUDED.streaming,
                        uses_tools = EXCLUDED.uses_tools,
                        uses_vision = EXCLUDED.uses_vision,
                        uses_structured_output = EXCLUDED.uses_structured_output,
                        uses_reasoning = EXCLUDED.uses_reasoning,
                        canonical_messages = COALESCE(EXCLUDED.canonical_messages, inference_requests.canonical_messages),
                        raw_request = COALESCE(EXCLUDED.raw_request, inference_requests.raw_request)
                    "#,
                )
                .bind(request_id)
                .bind(timestamp)
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
                .execute(pool)
                .await?;

                for tag in tags {
                    let _ = sqlx::query(
                        r#"INSERT INTO request_tags (request_id, tag) VALUES ($1, $2)"#,
                    )
                    .bind(request_id)
                    .bind(tag)
                    .execute(pool)
                    .await;
                }
            }

            TelemetryEvent::RouteSelected {
                request_id,
                timestamp,
                decision,
            } => {
                // Ensure parent record exists
                let _ = sqlx::query(
                    r#"INSERT INTO inference_requests (request_id, requested_model) VALUES ($1, $2) ON CONFLICT (request_id) DO NOTHING"#,
                )
                .bind(request_id)
                .bind(&decision.requested_model)
                .execute(pool)
                .await;

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
                .execute(pool)
                .await?;
            }

            TelemetryEvent::ProviderAttemptStarted {
                request_id,
                attempt_number,
                timestamp,
                provider,
                model,
            } => {
                let _ = sqlx::query(
                    r#"INSERT INTO inference_requests (request_id, requested_model) VALUES ($1, $2) ON CONFLICT (request_id) DO NOTHING"#,
                )
                .bind(request_id)
                .bind(model)
                .execute(pool)
                .await;

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
                .execute(pool)
                .await?;
            }

            TelemetryEvent::ProviderConnected {
                request_id,
                attempt_number,
                connection_latency_us,
                ..
            } => {
                let _ = sqlx::query(
                    r#"
                    UPDATE provider_attempts
                    SET connection_latency_us = $3
                    WHERE request_id = $1 AND attempt_number = $2
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(*connection_latency_us as i64)
                .execute(pool)
                .await;
            }

            TelemetryEvent::FirstProviderByte { .. } => {}

            TelemetryEvent::FirstGenerationChunk { .. } => {}

            TelemetryEvent::StreamChunk {
                request_id,
                attempt_number,
                timestamp,
                sequence,
                chunk_type,
                byte_count,
            } => {
                if self.store_stream_events {
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO stream_events (
                            request_id, attempt_number, timestamp, sequence, chunk_type, byte_count
                        ) VALUES ($1, $2, $3, $4, $5, $6)
                        "#,
                    )
                    .bind(request_id)
                    .bind(*attempt_number as i32)
                    .bind(timestamp)
                    .bind(*sequence as i64)
                    .bind(chunk_type)
                    .bind(*byte_count as i32)
                    .execute(pool)
                    .await;
                }
            }

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
                        error_message = $9
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
                .execute(pool)
                .await?;

                // If attempt was not pre-inserted, insert now
                if res.rows_affected() == 0 {
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO provider_attempts (
                            request_id, attempt_number, timestamp, provider, model, status,
                            finish_reason, provider_response_id, upstream_provider, duration_us,
                            error_code, error_message
                        ) VALUES ($1, $2, $3, 'unknown', 'unknown', $4, $5, $6, $7, $8, $9, $10)
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
                    .execute(pool)
                    .await;
                }

                // If there's an error code, record in errors table
                if let Some(code) = error_code {
                    let _ = sqlx::query(
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
                    .execute(pool)
                    .await;
                }
            }

            TelemetryEvent::UsageRecorded {
                request_id,
                attempt_number,
                timestamp,
                usage,
            } => {
                sqlx::query(
                    r#"
                    INSERT INTO generation_usage (
                        request_id, attempt_number, timestamp, prompt_tokens, completion_tokens,
                        total_tokens, cached_tokens, reasoning_tokens, raw_provider_usage
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
                .execute(pool)
                .await?;
            }

            TelemetryEvent::GenerationContent {
                request_id,
                attempt_number,
                timestamp,
                content,
                reasoning_content,
                tool_calls,
                raw_response,
            } => {
                let raw_resp = if self.store_raw_bodies {
                    raw_response.as_ref()
                } else {
                    None
                };

                let visibility = if reasoning_content.is_some() {
                    "exposed"
                } else {
                    "none"
                };

                sqlx::query(
                    r#"
                    INSERT INTO generations (
                        request_id, attempt_number, timestamp, content, reasoning_content,
                        reasoning_visibility, tool_calls, raw_response
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    "#,
                )
                .bind(request_id)
                .bind(*attempt_number as i32)
                .bind(timestamp)
                .bind(content)
                .bind(reasoning_content.as_deref())
                .bind(visibility)
                .bind(tool_calls)
                .bind(raw_resp)
                .execute(pool)
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

                        let _ = sqlx::query(
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
                        .execute(pool)
                        .await;
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
                let _ = sqlx::query(
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
                .execute(pool)
                .await;
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
                let _ = sqlx::query(
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
                .execute(pool)
                .await;
            }
        }

        Ok(())
    }
}
