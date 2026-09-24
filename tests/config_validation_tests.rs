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
    // Every `RoutingPolicy` enum variant (src/routing/policy.rs) now has a
    // real dispatch arm in `Router::route` (docs/REMEDIATION_PLAN.md Phase 5
    // routing-policy work) — there is no longer a "recognized but
    // unimplemented" tier between this list and `unknown_routing_policy_is_rejected`'s
    // genuinely-unrecognized-name case.
    for policy in [
        "ordered",
        "round_robin",
        "lowest_latency",
        "fastest",
        "local_first",
        "cheapest",
        "highest_quality",
        "balanced",
    ] {
        let yaml = format!("routing:\n  default_policy: {policy}\n");
        assert!(parse(&yaml).is_ok(), "policy '{policy}' should be accepted");
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

#[test]
fn zero_max_concurrent_is_rejected() {
    // Some(0) would make the provider permanently at capacity — a
    // misconfiguration (meant "unset"), not a usable limit
    // (docs/REMEDIATION_PLAN.md Phase 5 hardening pass).
    let yaml = r#"
providers:
  llama:
    name: llama
    type: llama_cpp
    base_url: "http://localhost:8080"
    max_concurrent: 0
"#;
    let err = parse(yaml).unwrap_err();
    assert!(err.to_string().contains("llama"), "{err}");
    assert!(err.to_string().contains("max_concurrent"), "{err}");
}

#[test]
fn zero_rate_limit_is_rejected() {
    let yaml = r#"
providers:
  llama:
    name: llama
    type: llama_cpp
    base_url: "http://localhost:8080"
    rate_limit_per_sec: 0
"#;
    let err = parse(yaml).unwrap_err();
    assert!(err.to_string().contains("llama"), "{err}");
    assert!(err.to_string().contains("rate_limit_per_sec"), "{err}");
}

#[test]
fn out_of_range_degraded_error_rate_threshold_is_rejected() {
    // A value like 50 (meant as "50%") rather than 0.5 would silently never
    // trigger, and a negative value would trigger constantly — both are
    // config mistakes worth catching at startup rather than producing a
    // confusing or permanently-wrong health signal.
    for bad in ["50", "-0.1", "1.5"] {
        let yaml = format!(
            "providers:\n  llama:\n    name: llama\n    type: llama_cpp\n    base_url: \"http://localhost:8080\"\n    degraded_error_rate_threshold: {bad}\n"
        );
        let err = parse(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("degraded_error_rate_threshold"),
            "value {bad} should be rejected: {err}"
        );
    }
}

#[test]
fn in_range_degraded_error_rate_threshold_is_accepted() {
    for good in ["0.0", "0.5", "1.0"] {
        let yaml = format!(
            "providers:\n  llama:\n    name: llama\n    type: llama_cpp\n    base_url: \"http://localhost:8080\"\n    degraded_error_rate_threshold: {good}\n"
        );
        assert!(parse(&yaml).is_ok(), "value {good} should be accepted");
    }
}
