use ai_gateway::routing::fallback::{should_fallback, FallbackTrigger, RetryConfig};

#[test]
fn test_default_fallback_triggers() {
    let config = RetryConfig::default();

    // Rate limit
    assert!(should_fallback(&config, "rate_limit", Some(429)));
    assert!(should_fallback(&config, "rate_limit", None));
    assert!(should_fallback(&config, "other_error", Some(429)));

    // Server error
    assert!(should_fallback(&config, "server_error", Some(500)));
    assert!(should_fallback(&config, "server_error", Some(503)));

    // Unavailable / timeout
    assert!(should_fallback(&config, "timeout", None));
    assert!(should_fallback(&config, "unavailable", Some(502)));

    // Client error (400) should NOT trigger fallback
    assert!(!should_fallback(&config, "invalid_request_error", Some(400)));
    assert!(!should_fallback(&config, "unauthorized", Some(401)));
    assert!(!should_fallback(&config, "not_found", Some(404)));
}

#[test]
fn test_content_filter_trigger() {
    let mut config = RetryConfig::default();
    // Default does not include ContentFilter
    assert!(!should_fallback(&config, "content_filter", Some(451)));

    // Add ContentFilter
    config.fallback_on.push(FallbackTrigger::ContentFilter);
    assert!(should_fallback(&config, "content_filter", Some(451)));
    assert!(should_fallback(&config, "content_filter", None));
}

#[test]
fn test_custom_narrow_trigger_policy() {
    let config = RetryConfig {
        fallback_on: vec![FallbackTrigger::RateLimit],
        ..Default::default()
    };

    // Only rate limits trigger
    assert!(should_fallback(&config, "rate_limit", Some(429)));
    // Server errors do NOT trigger
    assert!(!should_fallback(&config, "server_error", Some(500)));
    assert!(!should_fallback(&config, "timeout", None));
}
