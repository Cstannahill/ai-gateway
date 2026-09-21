use serde::{Deserialize, Serialize};

// ─── Fallback Configuration ──────────────────────────────────────────────────
//
// Defines retry and fallback behavior when a provider fails.

/// Conditions that trigger fallback to the next candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTrigger {
    /// Request timed out.
    Timeout,
    /// Provider returned 429 rate limit.
    RateLimit,
    /// Provider is unreachable / connection refused.
    Unavailable,
    /// Provider returned 5xx error.
    ServerError,
    /// Provider returned a content filter / moderation block.
    ContentFilter,
}

impl FallbackTrigger {
    /// Check if an HTTP status code matches this trigger.
    pub fn matches_status(&self, status: u16) -> bool {
        match self {
            Self::Timeout => false, // Handled separately.
            Self::RateLimit => status == 429,
            Self::Unavailable => status == 502 || status == 503 || status == 504,
            Self::ServerError => status >= 500,
            Self::ContentFilter => status == 451,
        }
    }

    /// Check if an error kind matches this trigger.
    pub fn matches_error(&self, code: &str) -> bool {
        match self {
            Self::Timeout => code == "timeout" || code == "connect_timeout",
            Self::RateLimit => code == "rate_limit",
            Self::Unavailable => {
                code == "connection_refused"
                    || code == "dns_error"
                    || code == "unavailable"
            }
            Self::ServerError => code == "server_error",
            Self::ContentFilter => code == "content_filter",
        }
    }
}

/// Retry configuration for a routing chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum attempts per individual provider.
    #[serde(default = "default_max_attempts")]
    pub max_attempts_per_provider: u32,

    /// Conditions that trigger fallback to the next provider.
    #[serde(default = "default_triggers")]
    pub fallback_on: Vec<FallbackTrigger>,

    /// Base retry delay in milliseconds (for exponential backoff within a provider).
    #[serde(default = "default_retry_delay")]
    pub retry_delay_ms: u64,

    /// Maximum retry delay in milliseconds.
    #[serde(default = "default_max_retry_delay")]
    pub max_retry_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts_per_provider: default_max_attempts(),
            fallback_on: default_triggers(),
            retry_delay_ms: default_retry_delay(),
            max_retry_delay_ms: default_max_retry_delay(),
        }
    }
}

fn default_max_attempts() -> u32 {
    2
}

fn default_triggers() -> Vec<FallbackTrigger> {
    vec![
        FallbackTrigger::Timeout,
        FallbackTrigger::RateLimit,
        FallbackTrigger::Unavailable,
        FallbackTrigger::ServerError,
    ]
}

fn default_retry_delay() -> u64 {
    200
}

fn default_max_retry_delay() -> u64 {
    5000
}

/// Check if a provider error should trigger fallback.
pub fn should_fallback(config: &RetryConfig, error_code: &str, status_code: Option<u16>) -> bool {
    for trigger in &config.fallback_on {
        if trigger.matches_error(error_code) {
            return true;
        }
        if status_code.is_some_and(|status| trigger.matches_status(status)) {
            return true;
        }
    }
    false
}
