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
    round_robin_counters: dashmap::DashMap<String, std::sync::atomic::AtomicUsize>,
}

impl Router {
    pub fn new(default_policy: String) -> Self {
        Self { 
            default_policy,
            round_robin_counters: dashmap::DashMap::new(),
        }
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
            // User explicitly specified a provider — bypass capability and
            // priority filtering (that's the point of explicit addressing),
            // but NOT the administrative `enabled` kill switch — an operator
            // disabling a provider must not be overridable by request-level
            // routing hints. See ProviderRegistry::is_enabled's doc comment
            // and docs/REMEDIATION_PLAN.md Phase 1.4 / Audit A11.
            let model_id = &request.model;

            if !provider_registry.is_enabled(explicit_provider) {
                return Ok(RoutingPlan {
                    decision: RoutingDecision {
                        timestamp,
                        requested_model: model_id.clone(),
                        resolution_type: "explicit_provider".to_string(),
                        candidates_considered: 1,
                        candidates_rejected: 1,
                        rejection_reasons: vec![RejectionRecord {
                            provider: explicit_provider.clone(),
                            model: model_id.clone(),
                            reason: "provider_disabled_or_unregistered".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_provider".to_string()),
                    },
                    candidates: vec![],
                });
            }

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

        // For explicit model IDs, short-circuit (no capability/priority
        // filtering) but still enforce the administrative `enabled` switch —
        // see the identical rationale in Stage 1 above.
        if let ModelResolution::Explicit { ref provider, ref model } = resolved.resolution {
            if !provider_registry.is_enabled(provider) {
                return Ok(RoutingPlan {
                    decision: RoutingDecision {
                        timestamp,
                        requested_model: request.model.clone(),
                        resolution_type,
                        candidates_considered: 1,
                        candidates_rejected: 1,
                        rejection_reasons: vec![RejectionRecord {
                            provider: provider.clone(),
                            model: model.clone(),
                            reason: "provider_disabled_or_unregistered".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_model".to_string()),
                    },
                    candidates: vec![],
                });
            }

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
        let estimated_tokens = Self::estimate_request_tokens(request);
        let compatible: Vec<ModelCandidate> = available
            .into_iter()
            .filter(|c| {
                if !self.is_compatible(c, request, &resolved.capabilities) {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "missing_required_capability".to_string(),
                    });
                    return false;
                }
                if let Some(max_context) = c.max_context_length
                    && estimated_tokens > max_context
                {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "context_length_exceeded".to_string(),
                    });
                    return false;
                }
                true
            })
            .collect();

        if compatible.is_empty() {
            // Persist why nothing was selected instead of failing before any
            // RoutingDecision exists — total routing failure is exactly the
            // case where "why" is most worth keeping (docs/REMEDIATION_PLAN.md
            // Phase 1.4 / Audit CL02). The caller (route handlers) already
            // treats an empty `candidates` list as "all providers failed"
            // via the normal candidate loop, so this doesn't change
            // response behavior — only what gets recorded.
            let requested_policy = request
                .routing
                .as_ref()
                .and_then(|r| r.policy.as_ref())
                .unwrap_or(&self.default_policy)
                .clone();
            let rejected_count = rejection_reasons.len() as u32;

            return Ok(RoutingPlan {
                decision: RoutingDecision {
                    timestamp,
                    requested_model: request.model.clone(),
                    resolution_type,
                    candidates_considered: rejected_count,
                    candidates_rejected: rejected_count,
                    rejection_reasons,
                    selected_provider: String::new(),
                    selected_model: String::new(),
                    policy_applied: requested_policy,
                    short_circuit: None,
                },
                candidates: vec![],
            });
        }

        // ─── Stage 5: Apply routing policy ───────────────────────────────
        let policy = request
            .routing
            .as_ref()
            .and_then(|r| r.policy.as_ref())
            .unwrap_or(&self.default_policy)
            .clone();

        let mut candidates = compatible;

        match policy.as_str() {
            "lowest_latency" => {
                let statuses = provider_registry.all_statuses();
                let latency_map: std::collections::HashMap<String, f64> = statuses
                    .into_iter()
                    .filter_map(|s| s.avg_latency_ms.map(|l| (s.name, l)))
                    .collect();
                
                candidates.sort_by(|a, b| {
                    a.priority.cmp(&b.priority).then_with(|| {
                        let lat_a = latency_map.get(&a.provider).copied().unwrap_or(f64::MAX);
                        let lat_b = latency_map.get(&b.provider).copied().unwrap_or(f64::MAX);
                        lat_a.partial_cmp(&lat_b).unwrap_or(std::cmp::Ordering::Equal)
                    })
                });
            }
            "round_robin" => {
                let counter = self.round_robin_counters
                    .entry(request.model.clone())
                    .or_insert_with(|| std::sync::atomic::AtomicUsize::new(0));
                let offset = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                
                candidates.sort_by_key(|c| c.priority);
                
                // Group by priority and rotate each group
                if !candidates.is_empty() {
                    let mut i = 0;
                    while i < candidates.len() {
                        let current_priority = candidates[i].priority;
                        let mut j = i + 1;
                        while j < candidates.len() && candidates[j].priority == current_priority {
                            j += 1;
                        }
                        
                        let group_len = j - i;
                        if group_len > 1 {
                            let rotation = offset % group_len;
                            candidates[i..j].rotate_left(rotation);
                        }
                        
                        i = j;
                    }
                }
            }
            // "priority" is an accepted synonym for "ordered" (both mean
            // "sort candidates by their configured priority").
            "ordered" | "priority" => {
                candidates.sort_by_key(|c| c.priority);
            }
            other => {
                // `GatewayConfig::validate()` only checks that this is one
                // of the *recognized* policy names, not that it's
                // implemented — `local_first`/`cheapest`/`fastest`/
                // `highest_quality`/`balanced` are real `RoutingPolicy`
                // enum variants with no real dispatch arm here. Previously
                // any unrecognized-here name (including these) silently
                // fell back to ordered routing, with no telemetry
                // distinguishing "policy X requested" from "policy X
                // applied" (docs/REMEDIATION_PLAN.md Phase 1.5 / Audit
                // CL03). Silent substitution was the actual defect; failing
                // loudly here is correct even though it means these five
                // policies remain unusable until implemented.
                anyhow::bail!(
                    "routing policy '{}' is a recognized name but is not yet implemented (only 'ordered', 'round_robin', and 'lowest_latency' have real dispatch logic)",
                    other
                );
            }
        }

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

    /// Rough token-count estimate for the request's prompt plus its
    /// requested completion budget, used only for context-window admission
    /// filtering (Stage 4). This is a heuristic (~4 chars/token), not a real
    /// tokenizer — no tokenizer dependency exists in this project, and an
    /// approximate-but-conservative-ish estimate is enough to reject
    /// obviously-oversized requests before wasting a provider round-trip.
    /// See docs/ROUTING_AND_POLICIES.md for the documented limitation.
    fn estimate_request_tokens(request: &GatewayRequest) -> u64 {
        use crate::types::request::{ContentPart, MessageContent};

        let prompt_chars: u64 = request
            .messages
            .as_ref()
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|m| m.content.as_ref())
                    .map(|content| match content {
                        MessageContent::Text(text) => text.len() as u64,
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .map(|p| match p {
                                ContentPart::Text { text } => text.len() as u64,
                                ContentPart::ImageUrl { .. } => 0,
                            })
                            .sum(),
                    })
                    .sum()
            })
            .unwrap_or(0);

        let prompt_tokens = prompt_chars / 4;
        let completion_budget = request
            .max_completion_tokens
            .or(request.max_tokens)
            .unwrap_or(0);

        prompt_tokens + completion_budget
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
