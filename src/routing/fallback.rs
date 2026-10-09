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
                    // Connection dropped, or ended empty, after the response
                    // headers: the provider failed to serve this request.
                    || code == "stream_error"
                    || code == "empty_stream"
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
    // The provider cannot serve this endpoint at all (e.g. /v1/completions on
    // Gemini): trying the next candidate is always right, whatever
    // `fallback_on` says about transient failures.
    if error_code == "unsupported_endpoint" {
        return true;
    }
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

// ─── Retry pacing ────────────────────────────────────────────────────────────

/// What to do before retrying the same provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backoff {
    /// Sleep this long, then retry the same provider.
    Wait(std::time::Duration),
    /// The provider asked for a wait longer than we are willing to hold the
    /// client for: stop retrying it and let fallback pick the next candidate.
    GiveUp,
}

/// Delay before retry number `attempt + 1` (`attempt` is zero-based).
///
/// Exponential from `retry_delay_ms`, capped at `max_retry_delay_ms`, with
/// ±20% jitter so concurrent requests that failed together do not retry in
/// lockstep. `jitter` is a unit-interval sample (0.0..=1.0) passed in so the
/// function stays deterministic and unit-testable. When the provider sent a
/// `Retry-After`, the wait is at least that long; if it exceeds
/// `max_retry_after_ms` the answer is [`Backoff::GiveUp`] instead.
pub fn backoff_delay(
    config: &RetryConfig,
    attempt: u32,
    retry_after_ms: Option<u64>,
    max_retry_after_ms: u64,
    jitter: f64,
) -> Backoff {
    let exp = config
        .retry_delay_ms
        .saturating_mul(1u64 << attempt.min(16))
        .min(config.max_retry_delay_ms);
    let jittered = (exp as f64 * (0.8 + 0.4 * jitter.clamp(0.0, 1.0))) as u64;

    match retry_after_ms {
        Some(requested) if requested > max_retry_after_ms => Backoff::GiveUp,
        Some(requested) => Backoff::Wait(std::time::Duration::from_millis(jittered.max(requested))),
        None => Backoff::Wait(std::time::Duration::from_millis(jittered)),
    }
}

/// A unit-interval sample for [`backoff_delay`]'s `jitter`. Not
/// cryptographic — the random bits of a UUIDv7 are plenty to de-synchronize
/// retries and avoid a `rand` dependency.
pub fn jitter_sample() -> f64 {
    (uuid::Uuid::now_v7().as_u128() as u64 % 10_000) as f64 / 10_000.0
}

#[cfg(test)]
mod backoff_tests {
    use super::*;
    use std::time::Duration;

    fn cfg() -> RetryConfig {
        RetryConfig::default() // 200ms base, 5000ms cap
    }

    #[test]
    fn exponential_with_jitter_bounds() {
        // jitter 0.5 is the midpoint: exactly the nominal delay.
        assert_eq!(backoff_delay(&cfg(), 0, None, 10_000, 0.5), Backoff::Wait(Duration::from_millis(200)));
        assert_eq!(backoff_delay(&cfg(), 1, None, 10_000, 0.5), Backoff::Wait(Duration::from_millis(400)));
        // Extremes stay within ±20%.
        assert_eq!(backoff_delay(&cfg(), 0, None, 10_000, 0.0), Backoff::Wait(Duration::from_millis(160)));
        assert_eq!(backoff_delay(&cfg(), 0, None, 10_000, 1.0), Backoff::Wait(Duration::from_millis(240)));
    }

    #[test]
    fn exponential_is_capped_and_cannot_overflow() {
        assert_eq!(backoff_delay(&cfg(), 30, None, 10_000, 0.5), Backoff::Wait(Duration::from_millis(5000)));
        assert_eq!(backoff_delay(&cfg(), u32::MAX, None, 10_000, 0.5), Backoff::Wait(Duration::from_millis(5000)));
    }

    #[test]
    fn retry_after_raises_the_wait_to_at_least_the_requested_time() {
        assert_eq!(
            backoff_delay(&cfg(), 0, Some(3_000), 10_000, 0.5),
            Backoff::Wait(Duration::from_millis(3_000))
        );
        // A tiny Retry-After never shortens the normal backoff.
        assert_eq!(
            backoff_delay(&cfg(), 1, Some(10), 10_000, 0.5),
            Backoff::Wait(Duration::from_millis(400))
        );
    }

    #[test]
    fn retry_after_beyond_the_cap_gives_up_instead_of_holding_the_client() {
        assert_eq!(backoff_delay(&cfg(), 0, Some(60_000), 10_000, 0.5), Backoff::GiveUp);
        // Exactly at the cap is still honoured.
        assert_eq!(
            backoff_delay(&cfg(), 0, Some(10_000), 10_000, 0.5),
            Backoff::Wait(Duration::from_millis(10_000))
        );
    }

    #[test]
    fn jitter_sample_is_a_unit_interval_value() {
        for _ in 0..100 {
            let j = jitter_sample();
            assert!((0.0..1.0).contains(&j), "{j}");
        }
    }
}
