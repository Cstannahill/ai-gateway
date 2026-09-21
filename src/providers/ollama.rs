use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── Ollama Adapter ──────────────────────────────────────────────────────────
//
// Ollama exposes both a native API and an OpenAI-compatible layer.
// This adapter uses the OpenAI-compatible endpoints by default.
//
// Native Ollama API differences:
// - POST /api/generate (legacy)
// - POST /api/chat (native chat)
// - Different streaming format (JSON lines, not SSE)
// - Model management (pull, push, list, etc.)

pub struct OllamaAdapter {
    inner: OpenAICompatibleAdapter,
}

impl OllamaAdapter {
    pub fn new(config: &ProviderConfig) -> anyhow::Result<Self> {
        let capabilities = ModelCapabilities {
            tools: true,
            vision: true,
            reasoning: false,
            structured_output: true,
            streaming: true,
            embeddings: true,
            context_window: None,
            extra: Default::default(),
        };

        Ok(Self {
            inner: OpenAICompatibleAdapter::new(config, capabilities)?,
        })
    }
}

#[async_trait]
impl Provider for OllamaAdapter {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.inner.models().await
    }

    async fn health(&self) -> ProviderHealth {
        self.inner.health().await
    }

    async fn execute(
        &self,
        request: &GatewayRequest,
        model: &str,
    ) -> anyhow::Result<ProviderOutput> {
        // Uses OpenAI-compatible endpoints via the inner adapter.
        // TODO: Support native Ollama API as fallback.
        // TODO: Expose model management operations (pull, etc.).
        self.inner.execute(request, model).await
    }

    async fn embeddings(
        &self,
        model: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.inner.embeddings(model, input).await
    }
}
