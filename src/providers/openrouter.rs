use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── OpenRouter Adapter ──────────────────────────────────────────────────────
//
// Extends OpenAICompatibleAdapter with OpenRouter-specific features:
// - upstream_provider extraction from response headers (X-Provider)
// - Provider preference headers (X-Provider-Order, X-Provider-Allow, etc.)
// - Model routing metadata

pub struct OpenRouterAdapter {
    inner: OpenAICompatibleAdapter,
}

impl OpenRouterAdapter {
    pub fn new(config: &ProviderConfig) -> anyhow::Result<Self> {
        let capabilities = ModelCapabilities {
            tools: true,
            vision: true,
            reasoning: true,
            structured_output: true,
            streaming: true,
            embeddings: false,
            context_window: None,
            extra: Default::default(),
        };

        let mut cfg = config.clone();
        let mut headers = cfg.default_headers.unwrap_or_default();
        headers
            .entry("HTTP-Referer".to_string())
            .or_insert_with(|| "https://ai-gateway.local".to_string());
        headers
            .entry("X-Title".to_string())
            .or_insert_with(|| "AI Gateway".to_string());
        cfg.default_headers = Some(headers);

        Ok(Self {
            inner: OpenAICompatibleAdapter::new(&cfg, capabilities)?,
        })
    }

    /// Merge OpenRouter specific options (route, provider preferences) into request.
    fn enrich_request(&self, request: &GatewayRequest) -> GatewayRequest {
        let mut req = request.clone();
        if let Some(ref opts) = request.provider_options {
            let mut or_options = serde_json::Map::new();
            for key in &["openrouter", "open_router"] {
                if let Some(obj) = opts.get(*key).and_then(|val| val.as_object()) {
                    for (k, v) in obj {
                        or_options.insert(k.clone(), v.clone());
                    }
                }
            }
            if !or_options.is_empty() {
                let mut options_map = req.provider_options.unwrap_or_default();
                options_map.insert(self.name().to_string(), serde_json::Value::Object(or_options));
                req.provider_options = Some(options_map);
            }
        }
        req
    }
}

#[async_trait]
impl Provider for OpenRouterAdapter {
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
}
