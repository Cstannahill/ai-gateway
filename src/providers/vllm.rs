use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── vLLM Adapter ────────────────────────────────────────────────────────────
//
// Extends OpenAICompatibleAdapter with vLLM-specific parameters:
// - top_k, min_p, repetition_penalty
// - Guided decoding / structured output controls
// - Quantization-aware features

pub struct VllmAdapter {
    inner: OpenAICompatibleAdapter,
}

impl VllmAdapter {
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

    /// Merge vLLM specific options (top_k, repetition_penalty, guided_json, etc.) into request.
    fn enrich_request(&self, request: &GatewayRequest) -> GatewayRequest {
        let mut req = request.clone();
        if let Some(ref opts) = request.provider_options {
            let mut vllm_options = serde_json::Map::new();
            for key in &["vllm", "v_llm"] {
                if let Some(obj) = opts.get(*key).and_then(|val| val.as_object()) {
                    for (k, v) in obj {
                        vllm_options.insert(k.clone(), v.clone());
                    }
                }
            }
            if !vllm_options.is_empty() {
                let mut options_map = req.provider_options.unwrap_or_default();
                options_map.insert(self.name().to_string(), serde_json::Value::Object(vllm_options));
                req.provider_options = Some(options_map);
            }
        }
        req
    }
}

#[async_trait]
impl Provider for VllmAdapter {
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
