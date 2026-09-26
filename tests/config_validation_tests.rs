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

// ─── llama_server section ────────────────────────────────────────────────────

fn llama_yaml(extra: &str) -> String {
    // Cargo.toml stands in for an existing "binary" file; validation only
    // checks that the path is a real, non-batch file.
    let binary = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml").replace(std::path::MAIN_SEPARATOR, "/");
    format!("server:\n  host: 127.0.0.1\nllama_server:\n  enabled: true\n  binary: \"{binary}\"\n{extra}")
}

#[test]
fn disabled_llama_server_section_is_never_validated() {
    assert!(parse("llama_server:\n  enabled: false\n  binary: /nope\n  port: 0\n").is_ok());
}

#[test]
fn enabled_llama_server_requires_an_existing_binary() {
    let err = parse("server:\n  host: 127.0.0.1\nllama_server:\n  enabled: true\n").unwrap_err();
    assert!(err.to_string().contains("llama_server.binary"), "{err}");
    let err = parse("server:\n  host: 127.0.0.1\nllama_server:\n  enabled: true\n  binary: /definitely/not/here\n").unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn llama_server_batch_shim_binary_is_rejected() {
    let shim = std::env::temp_dir().join("ai_gateway_test_shim.cmd");
    std::fs::write(&shim, "@echo off\n").unwrap();
    let yaml = format!(
        "server:\n  host: 127.0.0.1\nllama_server:\n  enabled: true\n  binary: \"{}\"\n",
        shim.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
    );
    let err = parse(&yaml).unwrap_err();
    assert!(err.to_string().contains("batch shim"), "{err}");
    let _ = std::fs::remove_file(shim);
}

#[test]
fn enabled_llama_server_refuses_a_network_facing_gateway_bind_without_acknowledgement() {
    // Default server.host is 0.0.0.0 — process launching must not be
    // reachable from the network by accident.
    let binary = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml").replace(std::path::MAIN_SEPARATOR, "/");
    let yaml = format!("llama_server:\n  enabled: true\n  binary: \"{binary}\"\n");
    let err = parse(&yaml).unwrap_err();
    assert!(err.to_string().contains("not loopback"), "{err}");

    let acked = format!("{yaml}  allow_non_loopback: true\n");
    assert!(parse(&acked).is_ok());
}

#[test]
fn llama_server_valid_loopback_config_is_accepted_and_bad_numbers_rejected() {
    assert!(parse(&llama_yaml("")).is_ok());
    for bad in ["  port: 0\n", "  log_lines: 0\n", "  start_timeout_secs: 0\n"] {
        assert!(parse(&llama_yaml(bad)).is_err(), "{bad} should be rejected");
    }
    let err = parse(&llama_yaml("  host: 0.0.0.0\n")).unwrap_err();
    assert!(err.to_string().contains("llama_server.host"), "{err}");
}
