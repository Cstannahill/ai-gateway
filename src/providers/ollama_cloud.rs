use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── Ollama Cloud Adapter ────────────────────────────────────────────────────
//
// Same protocol as local Ollama but pointed at a cloud endpoint with API key.
// Extends OllamaAdapter's logic with cloud-specific config.

pub struct OllamaCloudAdapter {
    inner: OpenAICompatibleAdapter,
}

impl OllamaCloudAdapter {
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
impl Provider for OllamaCloudAdapter {
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
