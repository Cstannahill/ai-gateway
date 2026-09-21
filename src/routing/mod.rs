// ─── Routing Engine ──────────────────────────────────────────────────────────
//
// Deterministic routing only (no AI/ML classifiers in V1).
//
// Pipeline with explicit short-circuit stages:
//
//   Request
//     ├── explicit provider? ────────► short-circuit: use it directly
//     ├── explicit model ID? ────────► skip alias resolution, resolve providers
//     │
//     ▼ (alias resolution)
//     constraints → candidate deployments → filter unavailable
//     → filter incompatible → apply policy → ordered candidates
//
// Conceptual separation:
//   Model routing:    which model?
//   Provider routing: which deployment of that model?

pub mod fallback;
pub mod policy;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::model_registry::ModelResolution;
use crate::registry::{ModelRegistry, ProviderRegistry};
use crate::types::{GatewayRequest, ModelCandidate, ModelCapabilities};

/// The result of routing — an ordered list of candidates to try.
#[derive(Debug, Clone)]
pub struct RoutingPlan {
    /// The routing decision record (for telemetry).
    pub decision: RoutingDecision,
    /// Ordered candidates to attempt.
    pub candidates: Vec<ModelCandidate>,
}

/// Full routing reasoning — stored as telemetry data.
/// "Don't just store the final destination. Store why it became the destination."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub timestamp: DateTime<Utc>,

    /// The original model identifier from the request.
    pub requested_model: String,

    /// How the model was resolved (alias or explicit).
    pub resolution_type: String,

    /// Total candidates considered.
    pub candidates_considered: u32,

    /// Candidates that were rejected.
    pub candidates_rejected: u32,

    /// Reasons for rejection (provider → reason).
    pub rejection_reasons: Vec<RejectionRecord>,

    /// The selected primary candidate.
    pub selected_provider: String,
    pub selected_model: String,

    /// The routing policy that was applied.
    pub policy_applied: String,

    /// Whether a short-circuit was used.
    pub short_circuit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionRecord {
    pub provider: String,
    pub model: String,
    pub reason: String,
}

/// The routing engine.
pub struct Router {
    default_policy: String,
}

impl Router {
    pub fn new(default_policy: String) -> Self {
        Self { default_policy }
    }

    /// Route a request to an ordered list of provider/model candidates.
    ///
    /// Pipeline:
    /// 1. Check for explicit provider short-circuit
    /// 2. Resolve model (alias → candidates, or explicit → single candidate)
    /// 3. Filter unavailable providers
    /// 4. Filter capability-incompatible candidates
    /// 5. Apply routing policy
    /// 6. Record the decision
    pub fn route(
        &self,
        request: &GatewayRequest,
        model_registry: &ModelRegistry,
        provider_registry: &ProviderRegistry,
    ) -> anyhow::Result<RoutingPlan> {
        let timestamp = Utc::now();

        // ─── Stage 1: Explicit provider short-circuit ────────────────────
        if let Some(ref routing) = request.routing
            && let Some(ref explicit_provider) = routing.provider
        {
            // User explicitly specified a provider — bypass all routing.
            let model_id = &request.model;
            let candidate = ModelCandidate {
                provider: explicit_provider.clone(),
                model: model_id.clone(),
                priority: 0,
                capabilities: None,
                max_context_length: None,
            };

            return Ok(RoutingPlan {
                decision: RoutingDecision {
                    timestamp,
                    requested_model: model_id.clone(),
                    resolution_type: "explicit_provider".to_string(),
                    candidates_considered: 1,
                    candidates_rejected: 0,
                    rejection_reasons: vec![],
                    selected_provider: explicit_provider.clone(),
                    selected_model: model_id.clone(),
                    policy_applied: "short_circuit".to_string(),
                    short_circuit: Some("explicit_provider".to_string()),
                },
                candidates: vec![candidate],
            });
        }

        // ─── Stage 2: Model resolution ───────────────────────────────────
        let resolved = model_registry.resolve(&request.model)?;
        let resolution_type = match &resolved.resolution {
            ModelResolution::Alias(_) => "alias".to_string(),
            ModelResolution::Explicit { .. } => "explicit_model".to_string(),
        };

        // For explicit model IDs, short-circuit (no filtering/policy).
        if let ModelResolution::Explicit { ref provider, ref model } = resolved.resolution {
            return Ok(RoutingPlan {
                decision: RoutingDecision {
                    timestamp,
                    requested_model: request.model.clone(),
                    resolution_type,
                    candidates_considered: 1,
                    candidates_rejected: 0,
                    rejection_reasons: vec![],
                    selected_provider: provider.clone(),
                    selected_model: model.clone(),
                    policy_applied: "short_circuit".to_string(),
                    short_circuit: Some("explicit_model".to_string()),
                },
                candidates: resolved.candidates,
            });
        }

        // ─── Stage 3: Filter unavailable providers ───────────────────────
        let mut rejection_reasons = Vec::new();
        let available: Vec<ModelCandidate> = resolved
            .candidates
            .into_iter()
            .filter(|c| {
                if provider_registry.is_available(&c.provider) {
                    true
                } else {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "provider_unavailable".to_string(),
                    });
                    false
                }
            })
            .collect();

        // ─── Stage 4: Filter capability-incompatible ─────────────────────
        let compatible: Vec<ModelCandidate> = available
            .into_iter()
            .filter(|c| {
                if self.is_compatible(c, request, &resolved.capabilities) {
                    true
                } else {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "missing_required_capability".to_string(),
                    });
                    false
                }
            })
            .collect();

        if compatible.is_empty() {
            anyhow::bail!(
                "No compatible providers available for model '{}'. {} candidates rejected.",
                request.model,
                rejection_reasons.len()
            );
        }

        // ─── Stage 5: Apply routing policy ───────────────────────────────
        let policy = request
            .routing
            .as_ref()
            .and_then(|r| r.policy.as_ref())
            .unwrap_or(&self.default_policy)
            .clone();

        // For V1, "ordered" policy = use candidates in priority order.
        // Other policies (cheapest, fastest, etc.) are post-V1.
        let candidates = compatible;

        let total_considered = candidates.len() as u32 + rejection_reasons.len() as u32;

        let decision = RoutingDecision {
            timestamp,
            requested_model: request.model.clone(),
            resolution_type,
            candidates_considered: total_considered,
            candidates_rejected: rejection_reasons.len() as u32,
            rejection_reasons,
            selected_provider: candidates
                .first()
                .map(|c| c.provider.clone())
                .unwrap_or_default(),
            selected_model: candidates
                .first()
                .map(|c| c.model.clone())
                .unwrap_or_default(),
            policy_applied: policy,
            short_circuit: None,
        };

        Ok(RoutingPlan {
            decision,
            candidates,
        })
    }

    /// Check if a candidate supports the request's requirements.
    fn is_compatible(
        &self,
        candidate: &ModelCandidate,
        request: &GatewayRequest,
        model_capabilities: &ModelCapabilities,
    ) -> bool {
        let caps = candidate
            .capabilities
            .as_ref()
            .unwrap_or(model_capabilities);

        // Check required capabilities.
        if request.tools.is_some() && !caps.tools {
            return false;
        }
        if request.response_format.is_some() && !caps.structured_output {
            return false;
        }
        if request.reasoning.as_ref().is_some_and(|r| r.enabled) && !caps.reasoning {
            return false;
        }
        // Check for vision content in messages.
        if self.requires_vision(request) && !caps.vision {
            return false;
        }

        true
    }

    /// Check if the request contains vision/image content.
    pub fn requires_vision(&self, request: &GatewayRequest) -> bool {
        use crate::types::request::{ContentPart, MessageContent};

        request
            .messages
            .as_ref()
            .map(|msgs| {
                msgs.iter().any(|m| {
                    matches!(
                        &m.content,
                        Some(MessageContent::Parts(parts)) if parts.iter().any(|p| matches!(p, ContentPart::ImageUrl { .. }))
                    )
                })
            })
            .unwrap_or(false)
    }
}
