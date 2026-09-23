use serde::{Deserialize, Serialize};

// ─── Routing Policies ────────────────────────────────────────────────────────
//
// V1 supports "ordered" (priority-based) only.
// Post-V1: cheapest, fastest, highest-quality, balanced, local-first.

/// Available routing policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPolicy {
    /// Use candidates in configured priority order (V1 default).
    Ordered,
    /// Distributes requests evenly across equal-priority candidates.
    RoundRobin,
    /// Select the candidate whose provider has the lowest exponential moving average latency.
    LowestLatency,
    /// Prefer local providers, fall back to cloud.
    LocalFirst,
    /// Select cheapest available provider.
    Cheapest,
    /// Select lowest-latency provider (alias for LowestLatency, kept for compatibility if needed).
    Fastest,
    /// Select highest-quality provider.
    HighestQuality,
    /// Balance cost, latency, and quality.
    Balanced,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self::Ordered
    }
}

impl std::fmt::Display for RoutingPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ordered => write!(f, "ordered"),
            Self::RoundRobin => write!(f, "round_robin"),
            Self::LowestLatency => write!(f, "lowest_latency"),
            Self::LocalFirst => write!(f, "local_first"),
            Self::Cheapest => write!(f, "cheapest"),
            Self::Fastest => write!(f, "fastest"),
            Self::HighestQuality => write!(f, "highest_quality"),
            Self::Balanced => write!(f, "balanced"),
        }
    }
}
