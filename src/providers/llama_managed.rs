//! The `llama` provider when the gateway also manages the llama-server process.
//!
//! Wraps the ordinary llama.cpp adapter so the provider and the manager act as
//! one thing instead of two things that happen to share a port:
//!
//! * `models()` lists the saved models (by alias), so they are selectable even
//!   while nothing is loaded.
//! * `health()` reports `Degraded` ("cold: can load on demand") instead of
//!   `Unhealthy` when no server is running but a saved model could be started.
//!   Routing only skips `Unhealthy` providers, so requests still reach us.
//! * `execute()` / `embeddings()` make sure the model the request names is the
//!   one loaded, swapping first when it is not (`llama_server.auto_swap`).
//!
//! What counts as "naming a saved model": an alias (case-insensitive) or the
//! `.gguf` path of a saved model. Any other name (e.g. the `qwen3.5-9b` in a
//! config alias) is passed through to whatever is loaded, exactly as before. A
//! `.gguf` path that is not saved is refused: requests never load arbitrary
//! files.
//!
//! Swapping kills the running server, so it must not happen while another
//! request is using it. Every request holds a read lock on a gate for as long
//! as its response is being produced (the lock rides inside the returned
//! stream); a swap takes the write lock, which waits for those to drain and
//! holds back new ones. See `acquire`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

use super::{Provider, ProviderOutput};
use crate::llama_server::{LlamaServerManager, Serving, State};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderHealth};

/// A request re-checks after each swap because another request may have
/// switched the model again in between; bound the ping-pong.
const MAX_SWAPS_PER_REQUEST: usize = 3;

/// How long to wait for the live server's own model list before falling back
/// to just the saved models.
const LIVE_MODELS_TIMEOUT: Duration = Duration::from_secs(2);

pub struct ManagedLlamaProvider {
    inner: Arc<dyn Provider>,
    manager: Arc<LlamaServerManager>,
    /// Read = a request is using the current model; write = swapping it.
    gate: Arc<RwLock<()>>,
}

impl ManagedLlamaProvider {
    pub fn new(inner: Arc<dyn Provider>, manager: Arc<LlamaServerManager>) -> Self {
        Self { inner, manager, gate: Arc::new(RwLock::new(())) }
    }

    /// Returns once the model named by `model` is being served (swapping to it
    /// if needed), holding a read lock the caller must keep until its response
    /// is finished.
    async fn acquire(&self, model: &str) -> anyhow::Result<OwnedRwLockReadGuard<()>> {
        let config = self.manager.config();

        for _ in 0..MAX_SWAPS_PER_REQUEST {
            // Read lock first, check second: a swap cannot start between our
            // check and our use of the server, because it needs the write lock.
            let read = self.gate.clone().read_owned().await;
            if !config.auto_swap {
                return Ok(read);
            }
            let Some(saved) = self.manager.resolve_saved(model).await.map_err(|e| anyhow::anyhow!("{}", e.message()))?
            else {
                return Ok(read); // not one of our saved models: pass through
            };
            if self.manager.serving(saved.id) == Serving::Ready {
                return Ok(read);
            }
            if self.manager.external_server_present().await {
                // Someone else's server owns the port; we cannot swap it.
                tracing::debug!(model, "llama-server was not started by this gateway; not switching models");
                return Ok(read);
            }

            drop(read);
            let drain = Duration::from_secs(config.swap_drain_timeout_secs);
            let write = tokio::time::timeout(drain, self.gate.write()).await.map_err(|_| {
                anyhow::anyhow!(
                    "llama-server is busy: requests on the current model did not finish within {}s, so it could not be switched to '{}'",
                    config.swap_drain_timeout_secs,
                    saved.alias
                )
            })?;

            tracing::info!(model = %saved.alias, "switching llama-server to the requested model");
            // Load on a detached task: if this request is cancelled or hits its
            // attempt timeout, the load still finishes and the retry finds the
            // model ready instead of restarting it.
            let manager = self.manager.clone();
            let id = saved.id;
            let loaded = tokio::spawn(async move { manager.ensure_loaded(id).await }).await;
            drop(write);

            match loaded {
                Ok(Ok(())) => continue, // re-acquire a read lock and re-verify
                Ok(Err(e)) => anyhow::bail!("{}", e.message()),
                Err(join) => anyhow::bail!("model switch task failed: {join}"),
            }
        }
        anyhow::bail!("llama-server kept switching models while '{model}' was being requested; try again")
    }
}

#[async_trait]
impl Provider for ManagedLlamaProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let provider = self.name().to_string();
        let mut models: Vec<ModelInfo> = self
            .manager
            .store()
            .list()
            .await
            .into_iter()
            .map(|m| {
                // Only what the saved model itself tells us: an mmproj means
                // it takes images. Everything else is unknown, i.e. false.
                let capabilities = ModelCapabilities {
                    streaming: true,
                    vision: m.mmproj_path.is_some(),
                    ..Default::default()
                };
                ModelInfo::new(&m.alias, vec![provider.clone()], Some(capabilities))
            })
            .collect();

        // Whatever the live server reports (the loaded alias, or a file name
        // when it was started without one). Failure is normal while stopped.
        if let Ok(Ok(live)) = tokio::time::timeout(LIVE_MODELS_TIMEOUT, self.inner.models()).await {
            for m in live {
                if !models.iter().any(|known| known.id.eq_ignore_ascii_case(&m.id)) {
                    models.push(m);
                }
            }
        }
        Ok(models)
    }

    async fn health(&self) -> ProviderHealth {
        match self.manager.state() {
            State::Ready => self.inner.health().await,
            State::Starting | State::Loading => ProviderHealth::Degraded,
            State::Stopped | State::Exited | State::Failed => {
                if self.manager.external_server_present().await {
                    self.inner.health().await
                } else if self.manager.config().auto_swap && !self.manager.store().list().await.is_empty() {
                    // Cold, not broken: a request can load a saved model.
                    ProviderHealth::Degraded
                } else {
                    ProviderHealth::Unhealthy
                }
            }
        }
    }

    async fn execute(&self, request: &GatewayRequest, model: &str) -> anyhow::Result<ProviderOutput> {
        let guard = self.acquire(model).await?;
        let output = self.inner.execute(request, model).await?;
        Ok(match output {
            // The read lock must live exactly as long as the response body.
            ProviderOutput::Stream(mut events) => ProviderOutput::Stream(Box::pin(async_stream::stream! {
                let _guard = guard;
                while let Some(event) = events.next().await {
                    yield event;
                }
            })),
            complete => complete,
        })
    }

    async fn embeddings(&self, model: &str, input: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let _guard = self.acquire(model).await?;
        self.inner.embeddings(model, input).await
    }
}
