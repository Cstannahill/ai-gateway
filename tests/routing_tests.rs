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
                rate_limit_per_sec: None,
                degraded_error_rate_threshold: None,
        circuit_breaker_threshold: None,
        circuit_breaker_cooldown_secs: 30,
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
                quality_tier: None,
                },
                ModelCandidate {
                    provider: "ollama".to_string(),
                    model: "qwen2.5-coder:7b".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                quality_tier: None,
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
async fn test_max_concurrent_blocks_explicit_addressing_too() {
    // `max_concurrent` is a resource-protection constraint, not a routing
    // preference — same rationale as the `enabled` kill switch, it must
    // not be bypassable via explicit `provider/model` addressing
    // (docs/REMEDIATION_PLAN.md Phase 5.3).
    let model_registry = ModelRegistry::new(HashMap::new());
    let mut provider_registry = ProviderRegistry::new();

    let mut config = create_provider_config("ollama", true);
    config.max_concurrent = Some(1);
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "ollama".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.record_request_start("ollama");

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "ollama/llama3.1:8b".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty());
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "ollama" && r.reason == "provider_at_capacity"),
        "expected explicit addressing to also be blocked by provider_at_capacity, got: {:?}",
        plan.decision.rejection_reasons
    );
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
                quality_tier: None,
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
                quality_tier: None,
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
                quality_tier: None,
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
                quality_tier: None,
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
                quality_tier: None,
                },
                ModelCandidate {
                    provider: "enabled-p".to_string(),
                    model: "m2".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                quality_tier: None,
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
async fn test_max_concurrent_filtering() {
    // `max_concurrent` was previously parsed into `ProviderConfig` and
    // never read anywhere — accepted, then silently ignored
    // (docs/REMEDIATION_PLAN.md Phase 5.3). A provider already at its
    // configured ceiling must be filtered out with a distinct
    // "provider_at_capacity" reason, and the next candidate selected.
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "saturated".to_string(),
                    model: "m1".to_string(),
                    priority: 5,
                    capabilities: None,
                    max_context_length: None,
                quality_tier: None,
                },
                ModelCandidate {
                    provider: "roomy".to_string(),
                    model: "m2".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                quality_tier: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    let mut saturated_config = create_provider_config("saturated", true);
    saturated_config.max_concurrent = Some(1);
    provider_registry.register(
        saturated_config,
        Arc::new(MockProvider {
            name: "saturated".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    let mut roomy_config = create_provider_config("roomy", true);
    roomy_config.max_concurrent = Some(5);
    provider_registry.register(
        roomy_config,
        Arc::new(MockProvider {
            name: "roomy".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    // Bring "saturated" up to its configured limit; "roomy" stays well
    // under its own.
    provider_registry.record_request_start("saturated");
    provider_registry.record_request_start("roomy");

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].provider, "roomy");
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "saturated" && r.reason == "provider_at_capacity"),
        "expected saturated candidate to be rejected for provider_at_capacity, got: {:?}",
        plan.decision.rejection_reasons
    );
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
                    quality_tier: None,
                },
                ModelCandidate {
                    provider: "large-context".to_string(),
                    model: "large-m".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: Some(1_000_000),
                    quality_tier: None,
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
async fn test_unrecognized_policy_override_fails_loudly() {
    // Every real `RoutingPolicy` enum variant now has a dispatch arm
    // (docs/REMEDIATION_PLAN.md Phase 5 routing-policy work), so this test
    // now exercises a genuinely unrecognized name (a typo) rather than a
    // recognized-but-unimplemented one — a per-request override naming it
    // must still fail clearly rather than silently substitute ordered
    // routing (docs/REMEDIATION_PLAN.md Phase 1.5 / Audit CL03).
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
                quality_tier: None,
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
            policy: Some("quantum_vibes".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let result = router.route(&req, &model_registry, &provider_registry);
    assert!(result.is_err(), "expected an error for an unrecognized policy override");
    assert!(result.unwrap_err().to_string().contains("quantum_vibes"));
}

fn model_with_single_candidate(provider: &str) -> HashMap<String, ModelDefinition> {
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![ModelCandidate {
                provider: provider.to_string(),
                model: "m1".to_string(),
                priority: 5,
                capabilities: None,
                max_context_length: None,
                quality_tier: None,
            }],
            capabilities: ModelCapabilities::default(),
        },
    );
    models
}

#[tokio::test]
async fn test_circuit_breaker_opens_after_consecutive_failures() {
    // A provider with `circuit_breaker_threshold` configured must be
    // skipped by routing (rejection reason `provider_circuit_open`) once it
    // has failed that many times in a row, and must stay open until the
    // cooldown elapses (docs/REMEDIATION_PLAN.md Phase 5 circuit breaker
    // work).
    let model_registry = ModelRegistry::new(model_with_single_candidate("flaky"));
    let mut provider_registry = ProviderRegistry::new();

    let mut flaky_config = create_provider_config("flaky", true);
    flaky_config.circuit_breaker_threshold = Some(2);
    flaky_config.circuit_breaker_cooldown_secs = 300;
    provider_registry.register(
        flaky_config,
        Arc::new(MockProvider {
            name: "flaky".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    // One failure isn't enough to trip a threshold of 2.
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", true);
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "one failure must not open the circuit yet");

    // Second consecutive failure trips it, and it stays open (cooldown is
    // 5 minutes, far longer than this test takes to run).
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", true);
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty(), "expected the circuit to be open after 2 consecutive failures");
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "flaky" && r.reason == "provider_circuit_open"),
        "expected provider_circuit_open rejection, got: {:?}",
        plan.decision.rejection_reasons
    );

    // Still open on a second check — nothing implicitly resets it early.
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty(), "expected the circuit to remain open before cooldown elapses");
}

#[tokio::test]
async fn test_circuit_breaker_half_open_trial_recovers_the_circuit() {
    // With a zero-second cooldown, the very next routing check after the
    // circuit opens has already "elapsed" past cooldown, so the half-open
    // transition and its recovery are observable without sleeping in the
    // test.
    let model_registry = ModelRegistry::new(model_with_single_candidate("flaky"));
    let mut provider_registry = ProviderRegistry::new();

    let mut flaky_config = create_provider_config("flaky", true);
    flaky_config.circuit_breaker_threshold = Some(1);
    flaky_config.circuit_breaker_cooldown_secs = 0;
    provider_registry.register(
        flaky_config,
        Arc::new(MockProvider {
            name: "flaky".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    // Trip the circuit.
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", true);

    // The next routing check transitions Open -> HalfOpen (cooldown is
    // zero) and lets a trial candidate through rather than rejecting it.
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(
        plan.candidates.len(),
        1,
        "expected a half-open trial request to be allowed through once cooldown elapses"
    );

    // A successful trial closes the circuit and resets the failure count.
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", false);
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "expected the circuit to stay closed after a successful trial");
}

#[tokio::test]
async fn test_circuit_breaker_half_open_failure_reopens_the_circuit() {
    // A failing trial request must re-open the circuit with a fresh
    // cooldown rather than closing it or leaving it perpetually half-open.
    // Uses a real 1-second cooldown and a real sleep since the transition
    // is driven by wall-clock elapsed time, not a mockable clock.
    let model_registry = ModelRegistry::new(model_with_single_candidate("flaky"));
    let mut provider_registry = ProviderRegistry::new();

    let mut flaky_config = create_provider_config("flaky", true);
    flaky_config.circuit_breaker_threshold = Some(1);
    flaky_config.circuit_breaker_cooldown_secs = 1;
    provider_registry.register(
        flaky_config,
        Arc::new(MockProvider {
            name: "flaky".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    // Trip the circuit.
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", true);
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty(), "expected the circuit to be open immediately after tripping");

    // Wait out the 1-second cooldown, then let the half-open trial through
    // and fail it.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "expected the half-open trial to be let through after cooldown");
    provider_registry.record_request_start("flaky");
    provider_registry.record_request_end("flaky", true);

    // Immediately after the failed trial, the fresh cooldown has not
    // elapsed yet, so the circuit must reject again rather than staying
    // half-open or closing.
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(
        plan.candidates.is_empty(),
        "expected the failed trial to re-open the circuit with a fresh cooldown"
    );
}

#[tokio::test]
async fn test_circuit_breaker_blocks_explicit_addressing_too() {
    // Same rationale as `max_concurrent`'s equivalent test: a tripped
    // circuit is a reliability protection, not a routing preference, and
    // must not be bypassable via explicit `provider/model` addressing.
    let model_registry = ModelRegistry::new(HashMap::new());
    let mut provider_registry = ProviderRegistry::new();

    let mut config = create_provider_config("ollama", true);
    config.circuit_breaker_threshold = Some(1);
    config.circuit_breaker_cooldown_secs = 300;
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "ollama".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.record_request_start("ollama");
    provider_registry.record_request_end("ollama", true);

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "ollama/llama3.1:8b".to_string(),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty());
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "ollama" && r.reason == "provider_circuit_open"),
        "expected explicit addressing to also be blocked by provider_circuit_open, got: {:?}",
        plan.decision.rejection_reasons
    );
}

#[tokio::test]
async fn test_circuit_breaker_disabled_by_default() {
    // `circuit_breaker_threshold: None` (the default) must never open the
    // circuit no matter how many consecutive failures occur — matches
    // `max_concurrent`'s opt-in shape so existing configs see no behavior
    // change.
    let model_registry = ModelRegistry::new(HashMap::new());
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(
        create_provider_config("ollama", true),
        Arc::new(MockProvider {
            name: "ollama".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    for _ in 0..10 {
        provider_registry.record_request_start("ollama");
        provider_registry.record_request_end("ollama", true);
    }

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "ollama/llama3.1:8b".to_string(),
        ..Default::default()
    };
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "circuit breaker must stay disabled with no threshold configured");
}

#[tokio::test]
async fn test_cheapest_policy_prefers_lower_estimated_cost_and_deprioritizes_unpriced() {
    // "cheapest" must rank by real configured pricing, and a candidate with
    // no pricing entry must sort after every priced one rather than being
    // assumed free (docs/REMEDIATION_PLAN.md Phase 5 routing-policy work).
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "pricey".to_string(),
                    model: "big".to_string(),
                    priority: 5,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
                ModelCandidate {
                    provider: "cheap".to_string(),
                    model: "small".to_string(),
                    priority: 10,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
                ModelCandidate {
                    provider: "unpriced".to_string(),
                    model: "mystery".to_string(),
                    priority: 1,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();
    for name in ["pricey", "cheap", "unpriced"] {
        provider_registry.register(
            create_provider_config(name, true),
            Arc::new(MockProvider {
                name: name.to_string(),
                capabilities: ModelCapabilities::default(),
            }),
        );
    }

    let mut pricing = HashMap::new();
    pricing.insert(
        "pricey/big".to_string(),
        ai_gateway::config::PricingEntry {
            input_cost_per_1k: 10.0,
            output_cost_per_1k: 30.0,
        },
    );
    pricing.insert(
        "cheap/small".to_string(),
        ai_gateway::config::PricingEntry {
            input_cost_per_1k: 0.1,
            output_cost_per_1k: 0.2,
        },
    );

    let router = Router::new("priority".to_string()).with_pricing(pricing);
    let req = GatewayRequest {
        model: "model-1".to_string(),
        routing: Some(RoutingHints {
            provider: None,
            policy: Some("cheapest".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 3);
    assert_eq!(plan.candidates[0].provider, "cheap");
    assert_eq!(plan.candidates[1].provider, "pricey");
    assert_eq!(plan.candidates[2].provider, "unpriced", "unpriced candidate must sort last, not first or free");
}

#[tokio::test]
async fn test_local_first_policy_prefers_loopback_providers() {
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "cloud".to_string(),
                    model: "m1".to_string(),
                    priority: 1,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
                ModelCandidate {
                    provider: "local".to_string(),
                    model: "m2".to_string(),
                    priority: 20,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();

    let mut cloud_config = create_provider_config("cloud", true);
    cloud_config.base_url = url::Url::parse("https://api.example.com/v1").unwrap();
    provider_registry.register(
        cloud_config,
        Arc::new(MockProvider {
            name: "cloud".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    // create_provider_config already defaults to http://localhost:8080/v1.
    provider_registry.register(
        create_provider_config("local", true),
        Arc::new(MockProvider {
            name: "local".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        routing: Some(RoutingHints {
            provider: None,
            policy: Some("local_first".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(
        plan.candidates[0].provider, "local",
        "local_first must prefer the loopback provider even though its priority is worse"
    );
}

#[tokio::test]
async fn test_highest_quality_policy_prefers_lower_quality_tier_and_deprioritizes_undeclared() {
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "flagship".to_string(),
                    model: "m1".to_string(),
                    priority: 20,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: Some(1),
                },
                ModelCandidate {
                    provider: "budget".to_string(),
                    model: "m2".to_string(),
                    priority: 1,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: Some(5),
                },
                ModelCandidate {
                    provider: "undeclared".to_string(),
                    model: "m3".to_string(),
                    priority: 2,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: None,
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();
    for name in ["flagship", "budget", "undeclared"] {
        provider_registry.register(
            create_provider_config(name, true),
            Arc::new(MockProvider {
                name: name.to_string(),
                capabilities: ModelCapabilities::default(),
            }),
        );
    }

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        routing: Some(RoutingHints {
            provider: None,
            policy: Some("highest_quality".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 3);
    assert_eq!(plan.candidates[0].provider, "flagship", "lowest quality_tier value must be preferred");
    assert_eq!(plan.candidates[1].provider, "budget");
    assert_eq!(plan.candidates[2].provider, "undeclared", "undeclared quality_tier must sort last");
}

#[tokio::test]
async fn test_balanced_policy_prefers_the_candidate_that_wins_every_dimension() {
    // A deliberately clear-cut scenario: "best" wins on cost, latency, and
    // quality_tier all three, so it must come out on top of the Borda-style
    // rank sum regardless of how the three dimensions are weighted.
    let mut models = HashMap::new();
    models.insert(
        "model-1".to_string(),
        ModelDefinition {
            name: "model-1".to_string(),
            description: None,
            candidates: vec![
                ModelCandidate {
                    provider: "worst".to_string(),
                    model: "m1".to_string(),
                    priority: 1,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: Some(9),
                },
                ModelCandidate {
                    provider: "best".to_string(),
                    model: "m2".to_string(),
                    priority: 99,
                    capabilities: None,
                    max_context_length: None,
                    quality_tier: Some(1),
                },
            ],
            capabilities: ModelCapabilities::default(),
        },
    );

    let model_registry = ModelRegistry::new(models);
    let mut provider_registry = ProviderRegistry::new();
    for name in ["worst", "best"] {
        provider_registry.register(
            create_provider_config(name, true),
            Arc::new(MockProvider {
                name: name.to_string(),
                capabilities: ModelCapabilities::default(),
            }),
        );
    }
    // Give "best" better observed latency via a fast recorded attempt, and
    // "worst" a slow one.
    provider_registry.record_latency("best", 10);
    provider_registry.record_latency("worst", 5000);

    let mut pricing = HashMap::new();
    pricing.insert(
        "worst/m1".to_string(),
        ai_gateway::config::PricingEntry { input_cost_per_1k: 50.0, output_cost_per_1k: 50.0 },
    );
    pricing.insert(
        "best/m2".to_string(),
        ai_gateway::config::PricingEntry { input_cost_per_1k: 0.01, output_cost_per_1k: 0.01 },
    );

    let router = Router::new("priority".to_string()).with_pricing(pricing);
    let req = GatewayRequest {
        model: "model-1".to_string(),
        routing: Some(RoutingHints {
            provider: None,
            policy: Some("balanced".to_string()),
            candidates: None,
        }),
        ..Default::default()
    };

    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(
        plan.candidates[0].provider, "best",
        "expected the candidate winning cost, latency, and quality to rank first despite worse priority"
    );
}

#[tokio::test]
async fn test_rate_limit_blocks_candidates_once_the_window_is_exhausted() {
    // `rate_limit_per_sec` bounds *arrival rate* within a provider, distinct
    // from `max_concurrent`'s simultaneous-in-flight ceiling
    // (docs/REMEDIATION_PLAN.md Phase 5 rate limiting work).
    let model_registry = ModelRegistry::new(model_with_single_candidate("bursty"));
    let mut provider_registry = ProviderRegistry::new();

    let mut config = create_provider_config("bursty", true);
    config.rate_limit_per_sec = Some(2);
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "bursty".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );

    let router = Router::new("priority".to_string());
    let req = GatewayRequest {
        model: "model-1".to_string(),
        ..Default::default()
    };

    // First two requests within the window are admitted.
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "first request should be admitted");
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "second request should be admitted");

    // Third request in the same window is rejected.
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty(), "third request in the same window must be rate-limited");
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "bursty" && r.reason == "provider_rate_limited"),
        "expected provider_rate_limited rejection, got: {:?}",
        plan.decision.rejection_reasons
    );
}

#[tokio::test]
async fn test_rate_limit_resets_after_the_window_elapses() {
    let model_registry = ModelRegistry::new(model_with_single_candidate("bursty"));
    let mut provider_registry = ProviderRegistry::new();

    let mut config = create_provider_config("bursty", true);
    config.rate_limit_per_sec = Some(1);
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "bursty".to_string(),
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
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty(), "expected the second request in the same window to be rate-limited");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert_eq!(plan.candidates.len(), 1, "expected a new window to admit a request again");
}

#[tokio::test]
async fn test_rate_limit_blocks_explicit_addressing_too() {
    let model_registry = ModelRegistry::new(HashMap::new());
    let mut provider_registry = ProviderRegistry::new();

    let mut config = create_provider_config("ollama", true);
    config.rate_limit_per_sec = Some(1);
    provider_registry.register(
        config,
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
    let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
    assert!(plan.candidates.is_empty());
    assert!(
        plan.decision
            .rejection_reasons
            .iter()
            .any(|r| r.provider == "ollama" && r.reason == "provider_rate_limited"),
        "expected explicit addressing to also be blocked by provider_rate_limited, got: {:?}",
        plan.decision.rejection_reasons
    );
}

#[tokio::test]
async fn test_rate_limit_disabled_by_default() {
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

    for _ in 0..20 {
        let plan = router.route(&req, &model_registry, &provider_registry).unwrap();
        assert_eq!(plan.candidates.len(), 1, "rate limiter must stay disabled with no rate_limit_per_sec configured");
    }
}

#[tokio::test]
async fn test_degraded_error_rate_threshold_reports_degraded_after_enough_recent_failures() {
    // A provider can accept connections and pass a ping while genuinely
    // failing real inference requests — `degraded_error_rate_threshold`
    // surfaces that as `Degraded` in reported status even though the ping
    // still says `Healthy` (docs/REMEDIATION_PLAN.md Phase 5 health-scoring
    // work). Purely informational: it must not affect routing eligibility.
    let mut provider_registry = ProviderRegistry::new();
    let mut config = create_provider_config("flaky", true);
    config.degraded_error_rate_threshold = Some(0.5);
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "flaky".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.update_health("flaky", ProviderHealth::Healthy);

    // Fewer than RECENT_OUTCOMES_MIN_SAMPLES (5) failures: no override yet.
    for _ in 0..3 {
        provider_registry.record_request_start("flaky");
        provider_registry.record_request_end("flaky", true);
    }
    let status = provider_registry
        .all_statuses()
        .into_iter()
        .find(|s| s.name == "flaky")
        .unwrap();
    assert_eq!(status.health, ProviderHealth::Healthy, "too few samples yet, must not report Degraded");

    // Two more failures brings it to 5 samples, 100% error rate — well over
    // the 0.5 threshold.
    for _ in 0..2 {
        provider_registry.record_request_start("flaky");
        provider_registry.record_request_end("flaky", true);
    }
    let status = provider_registry
        .all_statuses()
        .into_iter()
        .find(|s| s.name == "flaky")
        .unwrap();
    assert_eq!(status.health, ProviderHealth::Degraded);
    assert!(
        provider_registry.is_available("flaky"),
        "Degraded must remain available for routing — this is informational only"
    );
}

#[tokio::test]
async fn test_degraded_error_rate_threshold_never_overrides_a_real_unhealthy_ping() {
    let mut provider_registry = ProviderRegistry::new();
    let mut config = create_provider_config("down", true);
    config.degraded_error_rate_threshold = Some(0.5);
    provider_registry.register(
        config,
        Arc::new(MockProvider {
            name: "down".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.update_health("down", ProviderHealth::Unhealthy);

    let status = provider_registry
        .all_statuses()
        .into_iter()
        .find(|s| s.name == "down")
        .unwrap();
    assert_eq!(
        status.health,
        ProviderHealth::Unhealthy,
        "a real ping-determined Unhealthy must never be reported as merely Degraded"
    );
}

#[tokio::test]
async fn test_degraded_error_rate_threshold_disabled_by_default() {
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register(
        create_provider_config("flaky", true),
        Arc::new(MockProvider {
            name: "flaky".to_string(),
            capabilities: ModelCapabilities::default(),
        }),
    );
    provider_registry.update_health("flaky", ProviderHealth::Healthy);

    for _ in 0..20 {
        provider_registry.record_request_start("flaky");
        provider_registry.record_request_end("flaky", true);
    }
    let status = provider_registry
        .all_statuses()
        .into_iter()
        .find(|s| s.name == "flaky")
        .unwrap();
    assert_eq!(
        status.health,
        ProviderHealth::Healthy,
        "with no degraded_error_rate_threshold configured, reported health must stay purely ping-based"
    );
}
