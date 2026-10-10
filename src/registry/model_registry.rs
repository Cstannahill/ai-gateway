use std::collections::HashMap;

use crate::types::{ModelCandidate, ModelCapabilities, ModelDefinition, ModelInfo};

// ─── Model Registry ─────────────────────────────────────────────────────────
//
// Resolves model identifiers to concrete deployments.
//
// Two addressing modes:
//   "coding-fast"                     → alias, resolved via config
//   "openrouter/anthropic/claude-..." → explicit, resolved by splitting on '/'
//
// Aliases enable policy. Explicit IDs enable reproducibility.

pub struct ModelRegistry {
    /// Alias → model definition. Swapped wholesale by a config reload
    /// ([`ModelRegistry::replace`]); a lookup in progress keeps the map it started on.
    models: std::sync::RwLock<std::sync::Arc<HashMap<String, ModelDefinition>>>,
}

impl ModelRegistry {
    pub fn new(models: HashMap<String, ModelDefinition>) -> Self {
        Self { models: std::sync::RwLock::new(std::sync::Arc::new(models)) }
    }

    /// Replace every alias (config hot-reload).
    pub fn replace(&self, models: HashMap<String, ModelDefinition>) {
        *self.models.write().unwrap() = std::sync::Arc::new(models);
    }

    fn snapshot(&self) -> std::sync::Arc<HashMap<String, ModelDefinition>> {
        self.models.read().unwrap().clone()
    }

    /// Resolve a model identifier to candidate deployments.
    ///
    /// - If the identifier matches a registered alias, return its candidates
    ///   sorted by priority.
    /// - If the identifier contains a '/', treat it as an explicit
    ///   `provider/model` reference.
    /// - Otherwise, return an error.
    pub fn resolve(&self, model_id: &str) -> anyhow::Result<ResolvedModel> {
        let models = self.snapshot();
        // Check for alias match first.
        if let Some(definition) = models.get(model_id) {
            let mut candidates = definition.candidates.clone();
            candidates.sort_by_key(|c| c.priority);

            return Ok(ResolvedModel {
                requested: model_id.to_string(),
                resolution: ModelResolution::Alias(model_id.to_string()),
                candidates,
                capabilities: definition.capabilities.clone(),
            });
        }

        // Check for explicit provider/model format.
        if let Some((provider, model)) = model_id.split_once('/') {
            let candidate = ModelCandidate {
                provider: provider.to_string(),
                model: model.to_string(),
                priority: 0,
                capabilities: None,
                max_context_length: None,
                quality_tier: None,
            };

            return Ok(ResolvedModel {
                requested: model_id.to_string(),
                resolution: ModelResolution::Explicit {
                    provider: provider.to_string(),
                    model: model.to_string(),
                },
                candidates: vec![candidate],
                capabilities: ModelCapabilities::default(),
            });
        }

        anyhow::bail!("Unknown model: '{}'. Not a registered alias or explicit provider/model ID.", model_id)
    }

    /// List all registered models (for /v1/models endpoint).
    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.snapshot()
            .iter()
            .map(|(name, def)| {
                let providers: Vec<String> =
                    def.candidates.iter().map(|c| c.provider.clone()).collect();
                ModelInfo::new(name, providers, Some(def.capabilities.clone()))
            })
            .collect()
    }

    /// Check if a model alias exists.
    #[allow(dead_code)]
    pub fn has_alias(&self, alias: &str) -> bool {
        self.snapshot().contains_key(alias)
    }

    /// Get a model definition by alias.
    #[allow(dead_code)]
    pub fn get(&self, alias: &str) -> Option<ModelDefinition> {
        self.snapshot().get(alias).cloned()
    }
}

/// Result of resolving a model identifier.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The original model identifier from the request.
    pub requested: String,
    /// How it was resolved.
    pub resolution: ModelResolution,
    /// Ordered candidate deployments.
    pub candidates: Vec<ModelCandidate>,
    /// Capabilities of the logical model.
    pub capabilities: ModelCapabilities,
}

/// How a model identifier was resolved.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ModelResolution {
    /// Resolved via a registered alias.
    Alias(String),
    /// Explicitly specified as provider/model.
    Explicit { provider: String, model: String },
}
