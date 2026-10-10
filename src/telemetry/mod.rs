// ─── Telemetry & Data Pipeline ────────────────────────────────────────────────
//
// First-class subsystem — not an afterthought.
//
// Design principle: **Store observations; calculate summaries.**
// Never permanently reduce raw data into aggregates.
//
// Architecture:
//
//   Provider chunk ────────┐
//                          ├───── client
//                          │
//                          └────► telemetry event
//                                    │
//                              bounded channel
//                                    │
//                                    ▼
//                             telemetry writer
//                                    │
//                             batch / transaction
//                                    │
//                                    ▼
//                                Postgres
//
// The database is NOT in the stream's critical path.

pub mod cost;
pub mod events;
pub mod health;
pub mod stream_instrumentation;
pub mod writer;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use crate::config::{PricingEntry, TelemetryConfig};

pub use events::TelemetryEvent;
pub use health::{TelemetryHealth, TelemetryMode};
pub use writer::TelemetryWriter;

/// Telemetry subsystem — manages the event channel and background writer.
pub struct TelemetryPipeline {
    /// Sender end of the telemetry event channel.
    sender: mpsc::Sender<TelemetryEvent>,
    /// Handle to the background writer task — awaited on shutdown so an
    /// unflushed batch isn't abandoned when the process exits
    /// (docs/REMEDIATION_PLAN.md Phase 0.5).
    writer_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shared, live pipeline health — see `docs/frontend/API_GAPS.md` item 8.
    health: Arc<TelemetryHealth>,
}

impl TelemetryPipeline {
    /// Create a new telemetry pipeline.
    ///
    /// If a database URL is provided, spawns a background writer task.
    /// Otherwise, telemetry events are logged but not persisted.
    pub async fn new(
        config: &TelemetryConfig,
        database_url: Option<&str>,
        pricing: HashMap<String, PricingEntry>,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel::<TelemetryEvent>(config.batch_size * 10);

        let (writer_handle, health) = if config.enabled {
            if let Some(db_url) = database_url {
                let writer = TelemetryWriter::new_with_fallback_log(
                    db_url,
                    config.pool_size,
                    config.batch_size,
                    config.flush_interval_ms,
                    config.store_raw_bodies,
                    config.store_stream_events,
                    config.fallback_log_path.clone(),
                    pricing,
                )
                .await?;

                // `TelemetryWriter` decides its own mode (Database vs.
                // Unavailable) based on whether it actually connected —
                // read that back rather than assuming Database here.
                let health = writer.health();
                (
                    Some(tokio::spawn(async move {
                        writer.run(receiver).await;
                    })),
                    health,
                )
            } else {
                tracing::warn!("Telemetry enabled but no database URL configured. Events will be logged only.");
                let health = Arc::new(TelemetryHealth::new(TelemetryMode::Unavailable));
                (
                    Some(tokio::spawn(async move {
                        Self::log_only_writer(receiver).await;
                    })),
                    health,
                )
            }
        } else {
            tracing::info!("Telemetry disabled.");
            let health = Arc::new(TelemetryHealth::new(TelemetryMode::Disabled));
            // Drain the channel silently.
            (
                Some(tokio::spawn(async move {
                    Self::drain_writer(receiver).await;
                })),
                health,
            )
        };

        Ok(Self {
            sender,
            writer_handle,
            health,
        })
    }

    /// Get a sender for submitting telemetry events.
    pub fn sender(&self) -> mpsc::Sender<TelemetryEvent> {
        self.sender.clone()
    }

    /// Get a handle to the shared, live pipeline health — see
    /// `docs/frontend/API_GAPS.md` item 8.
    pub fn health(&self) -> Arc<TelemetryHealth> {
        self.health.clone()
    }

    /// Consume the pipeline, close the channel, and wait for the background
    /// writer to flush any remaining buffered events and exit.
    ///
    /// This only closes *this* pipeline's own sender handle — callers must
    /// have already dropped every other clone of the sender (e.g. the one
    /// held by `AppState`) or the channel will not actually close and this
    /// will hang until the writer task is otherwise stopped.
    pub async fn shutdown(self) {
        let Self { sender, writer_handle, health: _ } = self;
        drop(sender);
        if let Some(handle) = writer_handle
            && let Err(e) = handle.await
        {
            tracing::warn!(error = %e, "Telemetry writer task did not shut down cleanly");
        }
    }

    /// Log-only writer (no database).
    async fn log_only_writer(mut receiver: mpsc::Receiver<TelemetryEvent>) {
        while let Some(event) = receiver.recv().await {
            tracing::debug!(
                event_type = %event.event_type(),
                request_id = %event.request_id(),
                "Telemetry event (no database)"
            );
        }
    }

    /// Silent drain (telemetry disabled).
    async fn drain_writer(mut receiver: mpsc::Receiver<TelemetryEvent>) {
        while receiver.recv().await.is_some() {
            // Discard silently.
        }
    }
}
