// ─── Model & Provider Registry ───────────────────────────────────────────────
//
// Configuration-driven registry for model identity, capability discovery,
// alias resolution, and provider health tracking.
//
// Supports both:
// - Aliases ("coding-fast", "long-context") → policy-based routing
// - Explicit IDs ("openrouter/anthropic/claude-...") → reproducibility

pub mod model_registry;
pub mod provider_registry;

pub use model_registry::ModelRegistry;
pub use provider_registry::ProviderRegistry;
