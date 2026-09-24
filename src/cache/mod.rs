// ─── Response Cache ──────────────────────────────────────────────────────────
//
// Exact-match caching for non-streaming chat completions
// (docs/REMEDIATION_PLAN.md Phase 5 caching work). Deliberately scoped to
// non-streaming requests only for v1 — see `docs/ROUTING_AND_POLICIES.md`'s
// caching section for why, and for the (semantic caching, streaming replay)
// left explicitly out of scope.
//
// This is a performance cache, not data of record: it lives only in process
// memory, is never persisted, and is lost on restart. Nothing about
// telemetry/analytics correctness depends on it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// A cached response body plus enough provenance to report an honest
/// `serving_provider`/`resolved_model` in telemetry on a cache hit, rather
/// than fabricating "no provider served this."
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub body: serde_json::Value,
    pub provider: String,
    pub model: String,
}

struct CacheEntry {
    response: CachedResponse,
    inserted_at: Instant,
}

/// In-memory exact-match response cache, keyed by a canonical serialization
/// of the semantically-relevant request fields (see `cache_key_for`).
pub struct ResponseCache {
    entries: DashMap<String, CacheEntry>,
    /// Insertion-order queue for capacity eviction — a plain `VecDeque`
    /// behind a `Mutex` rather than a full LRU: eviction here only needs to
    /// bound memory use for a personal single-user gateway, not optimize
    /// hit rate under contention, so the extra complexity of true
    /// least-recently-*used* tracking isn't worth it. Oldest-inserted (not
    /// oldest-*accessed*) is evicted first once `max_entries` is exceeded.
    insertion_order: Mutex<VecDeque<String>>,
    ttl: Duration,
    max_entries: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ResponseCache {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: DashMap::new(),
            insertion_order: Mutex::new(VecDeque::new()),
            ttl,
            max_entries: max_entries.max(1),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Look up a cache entry, treating one older than `ttl` as a miss (and
    /// evicting it) rather than ever returning stale content.
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let Some(entry) = self.entries.get(key) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if entry.inserted_at.elapsed() >= self.ttl {
            drop(entry);
            self.entries.remove(key);
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(entry.response.clone())
    }

    /// Insert a cache entry, evicting the oldest-inserted entry first if
    /// this would exceed `max_entries`.
    pub fn insert(&self, key: String, response: CachedResponse) {
        if !self.entries.contains_key(&key) {
            let mut order = self.insertion_order.lock().unwrap_or_else(|e| e.into_inner());
            order.push_back(key.clone());
            while order.len() > self.max_entries {
                if let Some(oldest) = order.pop_front() {
                    self.entries.remove(&oldest);
                } else {
                    break;
                }
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                response,
                inserted_at: Instant::now(),
            },
        );
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Build a canonical cache key from the semantically-relevant fields of a
/// request. Uses the whole serialized `GatewayRequest` minus fields that
/// vary per-request without changing what answer should come back
/// (`request_id`, `received_at`, `stream`) — deliberately not an explicit
/// per-field allowlist, since a hand-maintained list silently goes stale
/// every time a new request field is added (the exact class of bug this
/// project's audits have repeatedly flagged elsewhere). `serde_json::Value`
/// objects are backed by a `BTreeMap` in this project (the `preserve_order`
/// feature isn't enabled), so key order in the resulting string is always
/// sorted and therefore stable regardless of struct field declaration
/// order.
pub fn cache_key_for(request: &crate::types::GatewayRequest) -> Option<String> {
    let mut value = serde_json::to_value(request).ok()?;
    let obj = value.as_object_mut()?;
    obj.remove("request_id");
    obj.remove("received_at");
    obj.insert("stream".to_string(), serde_json::Value::Bool(false));
    serde_json::to_string(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GatewayRequest;

    fn dummy_response(tag: &str) -> CachedResponse {
        CachedResponse {
            body: serde_json::json!({"tag": tag}),
            provider: "p".to_string(),
            model: "m".to_string(),
        }
    }

    #[test]
    fn get_miss_then_hit_after_insert() {
        let cache = ResponseCache::new(Duration::from_secs(60), 10);
        assert!(cache.get("k").is_none());
        cache.insert("k".to_string(), dummy_response("a"));
        let hit = cache.get("k").unwrap();
        assert_eq!(hit.body, serde_json::json!({"tag": "a"}));
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn entry_expires_after_ttl() {
        let cache = ResponseCache::new(Duration::from_millis(10), 10);
        cache.insert("k".to_string(), dummy_response("a"));
        assert!(cache.get("k").is_some());
        std::thread::sleep(Duration::from_millis(30));
        assert!(cache.get("k").is_none(), "expired entry must be treated as a miss");
        assert_eq!(cache.len(), 0, "an expired entry must be evicted on lookup, not merely ignored");
    }

    #[test]
    fn oldest_inserted_entry_is_evicted_once_over_capacity() {
        let cache = ResponseCache::new(Duration::from_secs(60), 2);
        cache.insert("a".to_string(), dummy_response("a"));
        cache.insert("b".to_string(), dummy_response("b"));
        cache.insert("c".to_string(), dummy_response("c"));
        assert_eq!(cache.len(), 2);
        assert!(cache.get("a").is_none(), "oldest-inserted entry must be evicted first");
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn cache_key_excludes_request_id_and_received_at_but_includes_content() {
        let mut r1 = GatewayRequest {
            model: "m".to_string(),
            ..Default::default()
        };
        let mut r2 = r1.clone();
        // Different request_id/received_at (as every real request has) must
        // not affect the key.
        r2.request_id = uuid::Uuid::now_v7();
        r2.received_at += chrono::Duration::seconds(5);
        assert_eq!(cache_key_for(&r1), cache_key_for(&r2));

        r1.model = "different-model".to_string();
        assert_ne!(cache_key_for(&r1), cache_key_for(&r2), "different semantic content must change the key");
    }

    #[test]
    fn cache_key_ignores_the_stream_flag() {
        let mut r1 = GatewayRequest {
            model: "m".to_string(),
            stream: true,
            ..Default::default()
        };
        let r2 = GatewayRequest {
            stream: false,
            ..r1.clone()
        };
        r1.request_id = r2.request_id;
        r1.received_at = r2.received_at;
        assert_eq!(
            cache_key_for(&r1),
            cache_key_for(&r2),
            "the same logical request must cache-key the same whether or not streaming was requested"
        );
    }
}
