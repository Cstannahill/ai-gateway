use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ai_gateway::providers::events::ProviderEvent;
use ai_gateway::providers::{Provider, ProviderOutput};
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router;
use ai_gateway::types::{
    ContentPart, FunctionDef, GatewayRequest, ImageUrl, Message, MessageContent,
    ModelCandidate, ModelCapabilities, ModelDefinition, ModelInfo, ProviderConfig,
    ProviderHealth, ProviderType, Role, RoutingHints, Tool,
};

struct MockProvider {
    name: String,
    capabilities: ModelCapabilities,
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo::new(&self.name, vec![self.name.clone()], Some(self.capabilities.clone()))])
    }

    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Healthy
    }

    async fn execute(&self, _request: &GatewayRequest, _model: &str) -> anyhow::Result<ProviderOutput> {
        Ok(ProviderOutput::Complete(ProviderEvent::Completed {
            timestamp: chrono::Utc::now(),
            finish_reason: Some("stop".to_string()),
            provider_response_id: Some("mock-id".to_string()),
            upstream_provider: None,
            raw_response: None,
        }))
    }

    async fn embeddings(&self, _model: &str, _input: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("not supported in mock")
    }
}

fn create_provider_config(name: &str, enabled: bool) -> ProviderConfig {
    ProviderConfig {
        name: name.to_string(),
        r#type: ProviderType::OpenAiCompatible,
        base_url: url::Url::parse("http://localhost:8080/v1").unwrap(),
        api_key: None,
        api_key_env: None,
        default_headers: None,
        timeout_secs: 30,
        max_concurrent: None,
        enabled,
    }
}

#[tokio::test]
async fn test_alias_resolution_ordered_by_priority() {
    let mut models = HashMap::new();
    models.insert(
        "coding-fast".to_string(),
        ModelDefinition {
            name: "coding-fast".to_string(),
            description: Some("Fast coding model".to_string()),
            candidates: vec![
                ModelCandidate {
                    provider: "openrouter".to_string(),
                    model: "anthropic/claude-3-haiku".to_string(),
                    priority: 20,
                    capabilities: None,
                    max_context_length: None,
                },
                ModelCandidate {
                    provider: "ollama".to_string(),
                    model: "qwen2.5-coder:7b".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("ollama", true),
        Arc::new(MockProvider {
            name: "ollama".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.register(
        create_provider_config("openrouter", true),
        Arc::new(MockProvider {
            name: "openrouter".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "coding-fast".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 2);
    // Priority 10 (ollama) should be evaluated before priority 20 (openrouter)
    assert_eq!(plan.candidates[0].provider, "ollama");
    assert_eq!(plan.candidates[1].provider, "openrouter");
}

#[tokio::test]
async fn test_explicit_provider_model_routing() {
    let model_registry = ModelRegistry::new(HashMap::new());
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("ollama", true),
        Arc::new(MockProvider {
            name: "ollama".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "ollama/llama3.1:8b".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "ollama");
    assert_eq!(plan.candidates[0].model, "llama3.1:8b");
}

#[tokio::test]
async fn test_vision_filtering() {
    let mut models = HashMap::new();
    models.insert(
        "smart-model".to_string(),
        ModelDefinition {
            name: "smart-model".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "text-only-provider".to_string(),
                    model: "text-model".to_string(),
                    priority: 5,
                    capabilities: Some(ModelCapabilities {
                        vision: false,
                        ..Default::default()
                    }),
                    max_context_length: None,
                },
                ModelCandidate {
                    provider: "vision-provider".to_string(),
                    model: "vision-model".to_string(),
                    priority: 10,
                    capabilities: Some(ModelCapabilities {
                        vision: true,
                        ..Default::default()
                    }),
                    max_context_length: None,
                },
            ],
            capabilities: ModelCapabilities {
                vision: true,
                ..Default::default()
            },
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("text-only-provider", true),
        Arc::new(MockProvider {
            name: "text-only-provider".to_string(),
            capabilities: ModelCapabilities {
                vision: false,
                ..Default::default()
            },
        }),
    );
    provider_registry.register(
        create_provider_config("vision-provider", true),
        Arc::new(MockProvider {
            name: "vision-provider".to_string(),
            capabilities: ModelCapabilities {
                vision: true,
                ..Default::default()
            },
        }),
    );

    let router = Router::new("priority".to_string());

    // Request containing an image part
    let req = GatewayRequest {
        model: "smart-model".to_string(),
        messages: Some(vec![Message {
            role: Role::User,
            content: Some(MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Look at this".to_string(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "http://example.com/cat.png".to_string(),
                        detail: None,
                    },
                },
            ])),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    // Only vision-provider should remain
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "vision-provider");
}

#[tokio::test]
async fn test_tool_filtering() {
    let mut models = HashMap::new();
    models.insert(
        "tool-model".to_string(),
        ModelDefinition {
            name: "tool-model".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "no-tools".to_string(),
                    model: "no-tools-m".to_string(),
                    priority: 5,
                    capabilities: Some(ModelCapabilities {
                        tools: false,
                        ..Default::default()
                    }),
                    max_context_length: None,
                },
                ModelCandidate {
                    provider: "has-tools".to_string(),
                    model: "has-tools-m".to_string(),
                    priority: 10,
                    capabilities: Some(ModelCapabilities {
                        tools: true,
                        ..Default::default()
                    }),
                    max_context_length: None,
                },
            ],
            capabilities: ModelCapabilities {
                tools: true,
                ..Default::default()
            },
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("no-tools", true),
        Arc::new(MockProvider {
            name: "no-tools".to_string(),
            capabilities: ModelCapabilities {
                tools: false,
                ..Default::default()
            },
        }),
    );
    provider_registry.register(
        create_provider_config("has-tools", true),
        Arc::new(MockProvider {
            name: "has-tools".to_string(),
            capabilities: ModelCapabilities {
                tools: true,
                ..Default::default()
            },
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "tool-model".to_string(),
        tools: Some(vec![Tool {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "get_weather".to_string(),
                description: None,
                parameters: None,
                strict: None,
            },
        }]),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "has-tools");
}

#[tokio::test]
async fn test_disabled_provider_filtered() {
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "disabled-p".to_string(),
                    model: "m1".to_string(),
                    priority: 5,
                    capabilities: None,
                    max_context_length: None,
                },
                ModelCandidate {
                    provider: "enabled-p".to_string(),
                    model: "m2".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("disabled-p", false), // disabled
        Arc::new(MockProvider {
            name: "disabled-p".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.register(
        create_provider_config("enabled-p", true), // enabled
        Arc::new(MockProvider {
            name: "enabled-p".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "enabled-p");
}

#[tokio::test]
async fn test_context_length_filtering() {
    // A candidate whose max_context_length is smaller than the request's
    // estimated prompt+completion token count must be rejected with a
    // distinct "context_length_exceeded" reason, not silently selected
    // (docs/REMEDIATION_PLAN.md Phase 2.3 context-window enforcement gap).
    let mut models = HashMap::new();
    models.insert(
        "long-context-model".to_string(),
        ModelDefinition {
            name: "long-context-model".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "small-context".to_string(),
                    model: "small-m".to_string(),
                    priority: 5,
                    capabilities: None,
                    max_context_length: Some(100),
                },
                ModelCandidate {
                    provider: "large-context".to_string(),
                    model: "large-m".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: Some(1_000_000),
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    provider_registry.register(
        create_provider_config("small-context", true),
        Arc::new(MockProvider {
            name: "small-context".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.register(
        create_provider_config("large-context", true),
        Arc::new(MockProvider {
            name: "large-context".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    // ~2000 chars of prompt content -> ~500 estimated tokens, which exceeds
    // the small-context candidate's 100-token limit but not the large one's.
    let long_text = "word ".repeat(400);
    let req = GatewayRequest {
        model: "long-context-model".to_string(),
        messages: Some(vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text(long_text)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }]),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "large-context");
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "small-context" && r.reason == "context_length_exceeded"),
        "expected small-context candidate to be rejected for context_length_exceeded, got: {:?}",
        plan.decision.rejection_reasons
    );
}

#[tokio::test]
async fn test_unimplemented_policy_override_fails_loudly() {
    // "cheapest" is a real `RoutingPolicy` enum variant with no dispatch
    // arm in `Router::route` — a per-request override naming it must fail
    // clearly rather than silently substitute ordered routing
    // (docs/REMEDIATION_PLAN.md Phase 1.5 / Audit CL03).
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![ModelCandidate {
                provider: "p".to_string(),
                model: "m".to_string(),
                priority: 1,
                capabilities: None,
                max_context_length: None,
            }],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(
        create_provider_config("p", true),
        Arc::new(MockProvider {
            name: "p".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        routing: Some(RoutingHints {
            provider: None,
            policy: Some("cheapest".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let result = router.route(&req, &model_registry, &provider_registry);
    assert!(result.is_err(), "expected an error for an unimplemented policy override");
    assert!(result.unwrap_err().to_string().contains("cheapest"));
}
