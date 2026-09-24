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
    /// Per-deployment pricing (`"provider/model"` → rate), used by the
    /// `cheapest`/`balanced` policies. Empty by default — set via
    /// `with_pricing` rather than a constructor parameter so the many
    /// existing `Router::new` call sites across tests (which never
    /// exercise cost-based routing) are unaffected.
    pricing: std::collections::HashMap<String, crate::config::PricingEntry>,
}

impl Router {
    pub fn new(default_policy: String) -> Self {
        Self {
            default_policy,
            round_robin_counters: dashmap::DashMap::new(),
            pricing: std::collections::HashMap::new(),
        }
    }

    /// Attach pricing data for the `cheapest`/`balanced` policies. A
    /// deployment with no entry here is simply excluded from cost-based
    /// ranking rather than assumed free — see `estimated_cost_usd`.
    pub fn with_pricing(mut self, pricing: std::collections::HashMap<String, crate::config::PricingEntry>) -> Self {
        self.pricing = pricing;
        self
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

            // A configured `max_concurrent` ceiling is a resource-protection
            // constraint, not a routing preference — same rationale as the
            // `enabled` check above, it must not be bypassable by explicit
            // addressing (docs/REMEDIATION_PLAN.md Phase 5.3).
            if provider_registry.is_at_capacity(explicit_provider) {
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
                            reason: "provider_at_capacity".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_provider".to_string()),
                    },
                    candidates: vec![],
                });
            }

            // A tripped circuit breaker is a reliability protection, same
            // rationale as the `enabled`/capacity checks above: explicit
            // addressing must not be able to hammer a provider the gateway
            // has already determined is repeatedly failing
            // (docs/REMEDIATION_PLAN.md Phase 5 circuit breaker work).
            if provider_registry.is_circuit_open(explicit_provider) {
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
                            reason: "provider_circuit_open".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_provider".to_string()),
                    },
                    candidates: vec![],
                });
            }

            // A configured `rate_limit_per_sec` bounds arrival rate, same
            // resource-protection rationale as the other two checks above
            // (docs/REMEDIATION_PLAN.md Phase 5 rate limiting work).
            if provider_registry.is_rate_limited(explicit_provider) {
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
                            reason: "provider_rate_limited".to_string(),
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
                quality_tier: None,
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

            // See the identical rationale in Stage 1's capacity check above
            // (docs/REMEDIATION_PLAN.md Phase 5.3).
            if provider_registry.is_at_capacity(provider) {
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
                            reason: "provider_at_capacity".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_model".to_string()),
                    },
                    candidates: vec![],
                });
            }

            // See the identical rationale in Stage 1's circuit-breaker check
            // above (docs/REMEDIATION_PLAN.md Phase 5 circuit breaker work).
            if provider_registry.is_circuit_open(provider) {
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
                            reason: "provider_circuit_open".to_string(),
                        }],
                        selected_provider: String::new(),
                        selected_model: String::new(),
                        policy_applied: "short_circuit".to_string(),
                        short_circuit: Some("explicit_model".to_string()),
                    },
                    candidates: vec![],
                });
            }

            // See the identical rationale in Stage 1's rate-limit check
            // above (docs/REMEDIATION_PLAN.md Phase 5 rate limiting work).
            if provider_registry.is_rate_limited(provider) {
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
                            reason: "provider_rate_limited".to_string(),
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
                if !provider_registry.is_available(&c.provider) {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "provider_unavailable".to_string(),
                    });
                    return false;
                }
                // A configured `max_concurrent` ceiling gets its own
                // rejection reason, distinct from "unavailable" — this is
                // transient load, not a config/health problem, and a
                // caller/dashboard should be able to tell them apart
                // (docs/REMEDIATION_PLAN.md Phase 5.3).
                if provider_registry.is_at_capacity(&c.provider) {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "provider_at_capacity".to_string(),
                    });
                    return false;
                }
                // See Stage 1's circuit-breaker check for rationale
                // (docs/REMEDIATION_PLAN.md Phase 5 circuit breaker work).
                if provider_registry.is_circuit_open(&c.provider) {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "provider_circuit_open".to_string(),
                    });
                    return false;
                }
                // See Stage 1's rate-limit check for rationale
                // (docs/REMEDIATION_PLAN.md Phase 5 rate limiting work).
                if provider_registry.is_rate_limited(&c.provider) {
                    rejection_reasons.push(RejectionRecord {
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        reason: "provider_rate_limited".to_string(),
                    });
                    return false;
                }
                true
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
            // "fastest" is a real `RoutingPolicy` variant kept distinct
            // from "lowest_latency" in the enum for naming-preference
            // reasons only (docs/routing/policy.rs) — both mean the exact
            // same thing here.
            "lowest_latency" | "fastest" => {
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
            "local_first" => {
                // Real signal (the candidate's own configured `base_url`
                // host), not a guess by provider type — see
                // `ProviderConfig::is_local`'s doc comment.
                candidates.sort_by(|a, b| {
                    let local_a = !provider_registry.is_local(&a.provider);
                    let local_b = !provider_registry.is_local(&b.provider);
                    local_a.cmp(&local_b).then_with(|| a.priority.cmp(&b.priority))
                });
            }
            "cheapest" => {
                // Unpriced candidates (no `pricing` entry) sort after every
                // priced one — see `estimated_cost_usd`'s doc comment for
                // why "no data" isn't treated as "free".
                candidates.sort_by(|a, b| {
                    let cost_a = self.estimated_cost_usd(a, request).unwrap_or(f64::MAX);
                    let cost_b = self.estimated_cost_usd(b, request).unwrap_or(f64::MAX);
                    cost_a
                        .partial_cmp(&cost_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.priority.cmp(&b.priority))
                });
            }
            "highest_quality" => {
                // Operator-declared `quality_tier` only — see
                // `ModelCandidate::quality_tier`'s doc comment for why this
                // gateway doesn't infer quality on its own. Undeclared
                // sorts last, not first or randomly.
                candidates.sort_by(|a, b| {
                    let q_a = a.quality_tier.unwrap_or(u32::MAX);
                    let q_b = b.quality_tier.unwrap_or(u32::MAX);
                    q_a.cmp(&q_b).then_with(|| a.priority.cmp(&b.priority))
                });
            }
            "balanced" => {
                // Combines cost, latency, and quality via Borda-style rank
                // summing rather than a weighted formula with arbitrary
                // normalization constants: each candidate's *position* if
                // the full set were sorted independently by each dimension
                // becomes that dimension's rank (0 = best), and the three
                // ranks are summed. A candidate missing data for a
                // dimension (no pricing entry, no latency samples yet, no
                // declared quality_tier) is sorted last on that dimension
                // and so gets the worst rank for it, rather than being
                // excluded or assumed average — same "don't fabricate" rule
                // as the single-dimension policies above. Lowest total rank
                // wins; `priority` breaks remaining ties.
                let statuses = provider_registry.all_statuses();
                let latency_map: std::collections::HashMap<String, f64> = statuses
                    .into_iter()
                    .filter_map(|s| s.avg_latency_ms.map(|l| (s.name, l)))
                    .collect();

                let costs: Vec<f64> = candidates
                    .iter()
                    .map(|c| self.estimated_cost_usd(c, request).unwrap_or(f64::MAX))
                    .collect();
                let latencies: Vec<f64> = candidates
                    .iter()
                    .map(|c| latency_map.get(&c.provider).copied().unwrap_or(f64::MAX))
                    .collect();
                let qualities: Vec<u32> = candidates
                    .iter()
                    .map(|c| c.quality_tier.unwrap_or(u32::MAX))
                    .collect();

                // `ranks_of(values)[i]` = how many candidates have a
                // strictly lower value than candidate `i` (0 = best on that
                // dimension); ties get equal ranks rather than an arbitrary
                // ordering.
                fn ranks_of_f64(values: &[f64]) -> Vec<usize> {
                    values
                        .iter()
                        .map(|v| values.iter().filter(|other| *other < v).count())
                        .collect()
                }
                fn ranks_of_u32(values: &[u32]) -> Vec<usize> {
                    values
                        .iter()
                        .map(|v| values.iter().filter(|other| *other < v).count())
                        .collect()
                }

                let cost_ranks = ranks_of_f64(&costs);
                let latency_ranks = ranks_of_f64(&latencies);
                let quality_ranks = ranks_of_u32(&qualities);
                let total_scores: Vec<usize> = (0..candidates.len())
                    .map(|i| cost_ranks[i] + latency_ranks[i] + quality_ranks[i])
                    .collect();

                let mut indexed: Vec<usize> = (0..candidates.len()).collect();
                indexed.sort_by(|&i, &j| {
                    total_scores[i]
                        .cmp(&total_scores[j])
                        .then_with(|| candidates[i].priority.cmp(&candidates[j].priority))
                });
                candidates = indexed.into_iter().map(|i| candidates[i].clone()).collect();
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
                // Every `RoutingPolicy` enum variant (src/routing/policy.rs)
                // now has a real dispatch arm above — this catches only a
                // genuinely unrecognized name (e.g. a typo), which must
                // fail loudly rather than silently substitute ordered
                // routing with no telemetry distinguishing "policy X
                // requested" from "policy X applied" (docs/REMEDIATION_PLAN.md
                // Phase 1.5 / Audit CL03).
                anyhow::bail!(
                    "routing policy '{}' is not a recognized policy name (expected one of: ordered, priority, round_robin, lowest_latency, fastest, local_first, cheapest, highest_quality, balanced)",
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
        let prompt_tokens = Self::estimate_prompt_tokens(request);
        let completion_budget = request
            .max_completion_tokens
            .or(request.max_tokens)
            .unwrap_or(0);

        prompt_tokens + completion_budget
    }

    /// Chars/4 heuristic over every text part of every message — the same
    /// estimate `estimate_request_tokens` uses for context-length filtering,
    /// factored out so `estimated_cost_usd` can combine it with a different
    /// completion-length assumption (see that function's doc comment).
    fn estimate_prompt_tokens(request: &GatewayRequest) -> u64 {
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

        prompt_chars / 4
    }

    /// Assumed completion length (tokens) for cost *ranking* purposes when
    /// the request doesn't bound its own output — deliberately distinct
    /// from `estimate_request_tokens`'s `unwrap_or(0)` for context-length
    /// checking, where assuming a nonzero completion could wrongly reject a
    /// candidate that would have fit. For ranking candidates against each
    /// other, using 0 here would make every unbounded request look free and
    /// defeat the point of `cheapest`, so a fixed assumption is used
    /// instead — applied identically to every candidate, so it only
    /// affects the *comparison*, not any billed/reported figure (matches
    /// `docs/API_REFERENCE.md`'s `/v1/analytics/costs` framing of estimated
    /// charges as estimates, never billed cost).
    const ASSUMED_COMPLETION_TOKENS_FOR_RANKING: u64 = 256;

    /// Estimated USD cost of running `request` on `candidate`, using the
    /// configured `pricing` table. Returns `None` when no pricing entry
    /// exists for this exact `provider/model` deployment — an unpriced
    /// candidate is excluded from cost comparison rather than assumed free
    /// (same "don't fabricate a number" rule `cost_summary` already
    /// follows for `unpriced_attempts`).
    fn estimated_cost_usd(&self, candidate: &ModelCandidate, request: &GatewayRequest) -> Option<f64> {
        let key = format!("{}/{}", candidate.provider, candidate.model);
        let entry = self.pricing.get(&key)?;
        let prompt_tokens = Self::estimate_prompt_tokens(request);
        let completion_tokens = request
            .max_completion_tokens
            .or(request.max_tokens)
            .unwrap_or(Self::ASSUMED_COMPLETION_TOKENS_FOR_RANKING);

        Some(
            (prompt_tokens as f64 / 1000.0) * entry.input_cost_per_1k
                + (completion_tokens as f64 / 1000.0) * entry.output_cost_per_1k,
        )
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
