use std::collections::HashMap;

use async_trait::async_trait;

use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{
    GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth, ProviderType,
};

// ─── OpenRouter Adapter ──────────────────────────────────────────────────────
//
// Extends OpenAICompatibleAdapter with OpenRouter-specific features:
// - upstream_provider extraction from response headers (X-Provider)
// - Provider preference headers (X-Provider-Order, X-Provider-Allow, etc.)
// - Model routing metadata
// - App attribution headers on every outbound request (see `apply_attribution`)

/// App URL sent as `HTTP-Referer`. OpenRouter uses it as the app's unique
/// identifier, so changing it starts a new app in their analytics. Override
/// per provider with `default_headers: { HTTP-Referer: "https://..." }`.
pub const DEFAULT_REFERER: &str = "https://ai-gateway.local";

/// App name sent as `X-OpenRouter-Title`. Override per provider with
/// `default_headers: { X-OpenRouter-Title: "..." }` (the legacy `X-Title`
/// name is honoured too).
pub const DEFAULT_TITLE: &str = "AI Gateway";

const REFERER_HEADER: &str = "HTTP-Referer";
const TITLE_HEADER: &str = "X-OpenRouter-Title";
/// Legacy spelling, still accepted by OpenRouter. Only ever set by an operator.
const LEGACY_TITLE_HEADER: &str = "X-Title";

fn has_header(headers: &HashMap<String, String>, name: &str) -> bool {
    headers.keys().any(|k| k.eq_ignore_ascii_case(name))
}

/// True when `base_url` points at OpenRouter (`openrouter.ai` or a subdomain).
pub fn is_openrouter_host(base_url: &url::Url) -> bool {
    base_url
        .host_str()
        .map(str::to_ascii_lowercase)
        .is_some_and(|h| h == "openrouter.ai" || h.ends_with(".openrouter.ai"))
}

/// Add OpenRouter's app-attribution headers to a provider config's
/// `default_headers` without overriding anything the operator set.
///
/// OpenRouter only creates an app (and only credits usage to it) when
/// `HTTP-Referer` is present, and requires a title for localhost-style URLs, so
/// both are always sent. Header names are compared case-insensitively: an
/// operator's `http-referer` or `X-Title` is respected and no second copy is
/// added (two spellings of one header would be sent in an unspecified order).
///
/// Optional headers are deliberately not defaulted; set them through
/// `default_headers` if wanted:
/// * `X-OpenRouter-Categories: cli-agent,personal-agent` (max 2 recognised)
/// * `X-OpenRouter-App-Visibility: hidden` (keep a *new* app off the public
///   rankings; ignored for an app that already exists)
///
/// Idempotent.
pub fn apply_attribution(config: &mut ProviderConfig) {
    let headers = config.default_headers.get_or_insert_with(HashMap::new);
    if !has_header(headers, REFERER_HEADER) {
        headers.insert(REFERER_HEADER.to_string(), DEFAULT_REFERER.to_string());
    }
    if !has_header(headers, TITLE_HEADER) && !has_header(headers, LEGACY_TITLE_HEADER) {
        headers.insert(TITLE_HEADER.to_string(), DEFAULT_TITLE.to_string());
    }
}

/// For a provider that is *not* typed `open_router` but whose `base_url` is
/// OpenRouter (e.g. a generic OpenAI-compatible entry), return a copy of the
/// config with attribution applied. `None` when nothing needs to change.
/// Used by `providers::build_adapter` so no route to OpenRouter goes
/// unattributed.
pub fn attributed_config_for_host(config: &ProviderConfig) -> Option<ProviderConfig> {
    if config.r#type == ProviderType::OpenRouter || !is_openrouter_host(&config.base_url) {
        return None;
    }
    let mut cfg = config.clone();
    apply_attribution(&mut cfg);
    Some(cfg)
}

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
        apply_attribution(&mut cfg);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(ty: ProviderType, base_url: &str, headers: &[(&str, &str)]) -> ProviderConfig {
        let mut cfg: ProviderConfig = serde_yaml::from_str(&format!(
            "name: or\ntype: {}\nbase_url: \"{base_url}\"\n",
            match ty {
                ProviderType::OpenRouter => "open_router",
                _ => "open_ai_compatible",
            }
        ))
        .unwrap();
        if !headers.is_empty() {
            cfg.default_headers =
                Some(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        }
        cfg
    }

    fn header<'a>(cfg: &'a ProviderConfig, name: &str) -> Vec<&'a str> {
        cfg.default_headers
            .iter()
            .flatten()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[test]
    fn defaults_send_referer_and_the_current_title_header() {
        let mut cfg = config(ProviderType::OpenRouter, "https://openrouter.ai/api", &[]);
        apply_attribution(&mut cfg);
        assert_eq!(header(&cfg, "HTTP-Referer"), [DEFAULT_REFERER]);
        assert_eq!(header(&cfg, "X-OpenRouter-Title"), [DEFAULT_TITLE]);
        assert!(header(&cfg, "X-Title").is_empty(), "legacy header is only sent if the operator sets it");
        assert!(header(&cfg, "X-OpenRouter-App-Visibility").is_empty(), "visibility is opt-in");
        assert!(header(&cfg, "X-OpenRouter-Categories").is_empty(), "categories are opt-in");
    }

    #[test]
    fn operator_values_win_regardless_of_header_name_case() {
        // Two spellings of one header would both end up in the map and be
        // sent in an unspecified order, so the default must not be added.
        let mut cfg = config(
            ProviderType::OpenRouter,
            "https://openrouter.ai/api",
            &[("http-referer", "https://me.example"), ("x-title", "My App")],
        );
        apply_attribution(&mut cfg);
        assert_eq!(header(&cfg, "HTTP-Referer"), ["https://me.example"]);
        assert_eq!(header(&cfg, "X-Title"), ["My App"]);
        assert!(header(&cfg, "X-OpenRouter-Title").is_empty(), "operator's legacy title suppresses the default");
    }

    #[test]
    fn apply_is_idempotent_and_keeps_other_headers() {
        let mut cfg = config(
            ProviderType::OpenRouter,
            "https://openrouter.ai/api",
            &[("X-OpenRouter-Categories", "cli-agent"), ("X-OpenRouter-App-Visibility", "hidden")],
        );
        apply_attribution(&mut cfg);
        let once = cfg.default_headers.clone();
        apply_attribution(&mut cfg);
        assert_eq!(cfg.default_headers, once);
        assert_eq!(header(&cfg, "X-OpenRouter-Categories"), ["cli-agent"]);
        assert_eq!(header(&cfg, "X-OpenRouter-App-Visibility"), ["hidden"]);
    }

    #[test]
    fn openrouter_host_detection() {
        for yes in [
            "https://openrouter.ai/api",
            "https://OpenRouter.ai/api/v1",
            "https://eu.openrouter.ai/api",
            "http://openrouter.ai:443",
        ] {
            assert!(is_openrouter_host(&url::Url::parse(yes).unwrap()), "{yes}");
        }
        for no in [
            "http://localhost:8080",
            "https://notopenrouter.ai/api",
            "https://openrouter.ai.evil.example/api",
            "https://example.com/openrouter.ai",
        ] {
            assert!(!is_openrouter_host(&url::Url::parse(no).unwrap()), "{no}");
        }
    }

    #[test]
    fn generic_provider_pointed_at_openrouter_gets_attributed_others_do_not() {
        let generic = config(ProviderType::OpenAiCompatible, "https://openrouter.ai/api", &[]);
        let attributed = attributed_config_for_host(&generic).expect("generic entry on openrouter.ai");
        assert_eq!(header(&attributed, "HTTP-Referer"), [DEFAULT_REFERER]);
        assert!(generic.default_headers.is_none(), "input config must not be mutated");

        assert!(attributed_config_for_host(&config(ProviderType::OpenAiCompatible, "http://localhost:1234", &[])).is_none());
        // The typed adapter applies attribution itself.
        assert!(attributed_config_for_host(&config(ProviderType::OpenRouter, "https://openrouter.ai/api", &[])).is_none());
    }
}
