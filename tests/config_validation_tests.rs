//! Tests for `GatewayConfig::validate()` — added alongside
//! docs/REMEDIATION_PLAN.md Phase 0.6 (reject config values that would
//! otherwise panic or silently misbehave deep inside the telemetry/routing
//! subsystems) and Phase 3.5 (pricing).

use ai_gateway::config::GatewayConfig;

fn parse(yaml: &str) -> anyhow::Result<GatewayConfig> {
    let config: GatewayConfig = serde_yaml::from_str(yaml)?;
    config.validate()?;
    Ok(config)
}

#[test]
fn default_config_is_valid() {
    assert!(parse("{}").is_ok());
}

#[test]
fn zero_batch_size_is_rejected() {
    let err = parse("telemetry:\n  batch_size: 0\n").unwrap_err();
    assert!(err.to_string().contains("batch_size"), "{err}");
}

#[test]
fn zero_flush_interval_is_rejected() {
    let err = parse("telemetry:\n  flush_interval_ms: 0\n").unwrap_err();
    assert!(err.to_string().contains("flush_interval_ms"), "{err}");
}

#[test]
fn zero_pool_size_is_rejected() {
    let err = parse("telemetry:\n  pool_size: 0\n").unwrap_err();
    assert!(err.to_string().contains("pool_size"), "{err}");
}

#[test]
fn unknown_routing_policy_is_rejected() {
    let err = parse("routing:\n  default_policy: quantum_vibes\n").unwrap_err();
    assert!(err.to_string().contains("quantum_vibes"), "{err}");
}

#[test]
fn implemented_routing_policies_are_accepted_as_default() {
    for policy in ["ordered", "round_robin", "lowest_latency"] {
        let yaml = format!("routing:\n  default_policy: {policy}\n");
        assert!(parse(&yaml).is_ok(), "policy '{policy}' should be accepted");
    }
}

#[test]
fn recognized_but_unimplemented_policies_are_rejected_as_default() {
    // These are real `RoutingPolicy` enum variants (src/routing/policy.rs)
    // but have no dispatch arm in `Router::route` yet. Accepting them as
    // `default_policy` would make every request fail at routing time
    // instead of failing loudly once at startup (Audit CL03).
    for policy in ["local_first", "cheapest", "fastest", "highest_quality", "balanced"] {
        let yaml = format!("routing:\n  default_policy: {policy}\n");
        let err = parse(&yaml).unwrap_err();
        assert!(err.to_string().contains(policy), "{err}");
    }
}

#[test]
fn unknown_fallback_trigger_is_rejected() {
    let err = parse("routing:\n  fallback_on:\n    - typo_trigger\n").unwrap_err();
    assert!(err.to_string().contains("typo_trigger"), "{err}");
}

#[test]
fn model_referencing_undefined_provider_is_rejected() {
    let yaml = r#"
models:
  alias:
    name: alias
    candidates:
      - provider: nonexistent
        model: m
"#;
    let err = parse(yaml).unwrap_err();
    assert!(err.to_string().contains("nonexistent"), "{err}");
}

#[test]
fn model_referencing_defined_provider_is_accepted() {
    let yaml = r#"
providers:
  llama:
    name: llama
    type: llama_cpp
    base_url: "http://localhost:8080"
models:
  alias:
    name: alias
    candidates:
      - provider: llama
        model: m
"#;
    assert!(parse(yaml).is_ok());
}

#[test]
fn negative_pricing_is_rejected() {
    let yaml = r#"
pricing:
  "llama/m":
    input_cost_per_1k: -0.01
    output_cost_per_1k: 0.02
"#;
    let err = parse(yaml).unwrap_err();
    assert!(err.to_string().contains("llama/m"), "{err}");
}

#[test]
fn nonnegative_pricing_is_accepted() {
    let yaml = r#"
pricing:
  "llama/m":
    input_cost_per_1k: 0.0
    output_cost_per_1k: 0.02
"#;
    assert!(parse(yaml).is_ok());
}
