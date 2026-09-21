use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── llama.cpp / llama-server Adapter ────────────────────────────────────────
//
// Extends OpenAICompatibleAdapter with llama.cpp-specific parameters:
// - min_p, repeat_penalty, top_k, typical_p, mirostat, etc.
// - Local GPU metrics extraction (slot, GPU memory, queue duration)

pub struct LlamaCppAdapter {
    inner: OpenAICompatibleAdapter,
}

impl LlamaCppAdapter {
    pub fn new(config: &ProviderConfig) -> anyhow::Result<Self> {
        let capabilities = ModelCapabilities {
            tools: true,
            vision: false,
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

    /// Merge llama.cpp specific options (min_p, repeat_penalty, top_k, mirostat) into request.
    fn enrich_request(&self, request: &GatewayRequest) -> GatewayRequest {
        let mut req = request.clone();
        if let Some(ref opts) = request.provider_options {
            let mut llama_options = serde_json::Map::new();
            for key in &["llama_cpp", "llama", "llama-server"] {
                if let Some(obj) = opts.get(*key).and_then(|val| val.as_object()) {
                    for (k, v) in obj {
                        llama_options.insert(k.clone(), v.clone());
                    }
                }
            }
            if !llama_options.is_empty() {
                let mut options_map = req.provider_options.unwrap_or_default();
                options_map.insert(self.name().to_string(), serde_json::Value::Object(llama_options));
                req.provider_options = Some(options_map);
            }
        }
        req
    }
}

#[async_trait]
impl Provider for LlamaCppAdapter {
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
        let enriched = self.enrich_request(request);
        self.inner.execute(&enriched, model).await
    }

    async fn embeddings(
        &self,
        model: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.inner.embeddings(model, input).await
    }
}
