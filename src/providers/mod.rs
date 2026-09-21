// ─── Provider Adapters ───────────────────────────────────────────────────────
//
// Trait-based adapter system. Each provider implements the `Provider` trait.
// Most providers extend `OpenAICompatibleAdapter` with config/capability overrides.

pub mod events;
pub mod gemini;
pub mod huggingface;
pub mod llama_cpp;
pub mod ollama;
pub mod ollama_cloud;
pub mod openai_compatible;
pub mod openrouter;
pub mod vllm;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderHealth};
use events::ProviderEvent;

/// The result of a provider execution — either a stream (chat/responses)
/// or a complete result (embeddings/models).
pub enum ProviderOutput {
    /// Streaming response for chat completions / responses API.
    Stream(Pin<Box<dyn Stream<Item = ProviderEvent> + Send>>),

    /// Complete (non-streaming) response for embeddings, model listing, etc.
    Complete(ProviderEvent),
}

/// Core provider trait. Every backend implements this.
///
/// ```text
/// Provider
/// │
/// ├── capabilities()   → what this provider supports
/// ├── models()         → list available models
/// ├── health()         → current health status
/// │
/// └── execute()        → dispatch request
///       │
///       ├── Streaming operations (chat, responses)
///       │     → ProviderOutput::Stream(Stream<ProviderEvent>)
///       │
///       └── Non-streaming operations (embeddings)
///             → ProviderOutput::Complete(ProviderEvent)
/// ```
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider's unique name (matches config key).
    fn name(&self) -> &str;

    /// What capabilities this provider supports.
    fn capabilities(&self) -> &ModelCapabilities;

    /// List models available on this provider.
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>>;

    /// Check provider health (connectivity, readiness).
    async fn health(&self) -> ProviderHealth;

    /// Execute a gateway request against this provider.
    ///
    /// For streaming operations (chat completions, responses API), returns
    /// `ProviderOutput::Stream` with per-chunk events.
    ///
    /// For non-streaming operations (embeddings), returns
    /// `ProviderOutput::Complete` with the full result.
    async fn execute(
        &self,
        request: &GatewayRequest,
        model: &str,
    ) -> anyhow::Result<ProviderOutput>;

    /// Execute an embeddings request against this provider.
    async fn embeddings(
        &self,
        _model: &str,
        _input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("Provider '{}' does not support embeddings", self.name())
    }
}
