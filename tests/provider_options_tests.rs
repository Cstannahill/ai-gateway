use std::collections::HashMap;
use url::Url;

use ai_gateway::providers::openai_compatible::OpenAICompatibleAdapter;
use ai_gateway::types::{GatewayRequest, ModelCapabilities, ProviderConfig, ProviderType};

#[test]
fn test_openai_compatible_provider_options_injection() {
    let config = ProviderConfig {
        name: "test-provider".to_string(),
        r#type: ProviderType::OpenAiCompatible,
        base_url: Url::parse("http://localhost:8080/v1").unwrap(),
        api_key: None,
        api_key_env: None,
        default_headers: None,
        timeout_secs: 30,
        max_concurrent: None,
        enabled: true,
    };

    let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();

    let mut provider_options = HashMap::new();
    provider_options.insert(
        "test-provider".to_string(),
        serde_json::json!({
            "min_p": 0.05,
            "repeat_penalty": 1.1,
            "top_k": 40
        }),
    );

    let req = GatewayRequest {
        model: "my-model".to_string(),
        provider_options: Some(provider_options),
        ..Default::default()
    };

    let body = adapter.build_chat_body(&req, "my-model");
    assert_eq!(body["model"], "my-model");
    assert_eq!(body["min_p"], 0.05);
    assert_eq!(body["repeat_penalty"], 1.1);
    assert_eq!(body["top_k"], 40);
}

#[test]
fn test_generic_openai_options_fallback() {
    let config = ProviderConfig {
        name: "custom-endpoint".to_string(),
        r#type: ProviderType::OpenAiCompatible,
        base_url: Url::parse("https://api.openai.com/v1").unwrap(),
        api_key: None,
        api_key_env: None,
        default_headers: None,
        timeout_secs: 30,
        max_concurrent: None,
        enabled: true,
    };

    let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();

    let mut provider_options = HashMap::new();
    provider_options.insert(
        "openai".to_string(),
        serde_json::json!({
            "service_tier": "auto",
            "parallel_tool_calls": false
        }),
    );

    let req = GatewayRequest {
        model: "gpt-4o".to_string(),
        provider_options: Some(provider_options),
        ..Default::default()
    };

    let body = adapter.build_chat_body(&req, "gpt-4o");
    assert_eq!(body["service_tier"], "auto");
    assert_eq!(body["parallel_tool_calls"], false);
}

#[test]
fn test_standard_parameters_preserved_with_options() {
    let config = ProviderConfig {
        name: "test-provider".to_string(),
        r#type: ProviderType::OpenAiCompatible,
        base_url: Url::parse("http://localhost:8080/v1").unwrap(),
        api_key: None,
        api_key_env: None,
        default_headers: None,
        timeout_secs: 30,
        max_concurrent: None,
        enabled: true,
    };

    let adapter = OpenAICompatibleAdapter::new(&config, ModelCapabilities::default()).unwrap();

    let mut provider_options = HashMap::new();
    provider_options.insert(
        "test-provider".to_string(),
        serde_json::json!({
            "guided_json": { "type": "object" }
        }),
    );

    let req = GatewayRequest {
        model: "structured-model".to_string(),
        temperature: Some(0.7),
        top_p: Some(0.95),
        max_tokens: Some(1024),
        stream: true,
        provider_options: Some(provider_options),
        ..Default::default()
    };

    let body = adapter.build_chat_body(&req, "structured-model");
    assert_eq!(body["model"], "structured-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["top_p"], 0.95);
    assert_eq!(body["max_tokens"], 1024);
    assert_eq!(body["guided_json"]["type"], "object");
}
