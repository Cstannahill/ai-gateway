//! Several API keys for one provider.
//!
//! A provider's key variable (`api_key_env`, or a static `api_key`) may hold
//! more than one key, comma-separated: `OPENROUTER_API_KEY=sk-a,sk-b`. Requests
//! are spread round-robin across the keys that are currently usable, and a key
//! the provider rejects (`429`, `401`, `403`) is benched for a while so the
//! same request can be retried on the next key at once — the free-tier rate
//! limits that motivate this are per key, not per provider.
//!
//! One key (or none) behaves exactly as before: a lone key is never skipped,
//! because there is nothing to rotate to and failing every request locally
//! would be worse than letting the provider answer.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How long a key that returned 401/403 stays benched. Long: a revoked or
/// mistyped key does not fix itself, but the operator may replace it without a
/// restart only via the environment, so it is not benched forever.
pub const REJECTED_KEY_BENCH: Duration = Duration::from_secs(600);
/// Bench time for a 429 that carried no `Retry-After`.
pub const DEFAULT_RATE_LIMIT_BENCH: Duration = Duration::from_secs(60);
/// A `Retry-After` is honoured up to this, however large the provider says.
pub const MAX_RATE_LIMIT_BENCH: Duration = Duration::from_secs(300);

/// A key chosen for one attempt. `index` identifies it for
/// [`KeyRing::bench`] and for logs — the key text itself is never logged.
#[derive(Debug, Clone)]
pub struct KeyHandle {
    pub index: usize,
    pub key: String,
}

#[derive(Debug)]
pub struct KeyRing {
    keys: Vec<String>,
    /// Per key: when it becomes usable again (`None` = usable now).
    benched_until: Mutex<Vec<Option<Instant>>>,
    next: AtomicUsize,
}

impl KeyRing {
    /// Split a key variable's value into keys: comma-separated, whitespace
    /// around each trimmed, empty entries ignored.
    pub fn parse(raw: &str) -> Vec<String> {
        raw.split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn new(keys: Vec<String>) -> Self {
        let benched_until = Mutex::new(vec![None; keys.len()]);
        Self {
            keys,
            benched_until,
            next: AtomicUsize::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The first key, ignoring benching — for callers that just need to build
    /// a header (tests, diagnostics).
    pub fn first(&self) -> Option<&str> {
        self.keys.first().map(String::as_str)
    }

    /// Next key for a fresh request: round-robin over the usable ones. If
    /// every key is benched, the one that recovers soonest (so a request is
    /// still sent and the provider gets to say what it thinks). `None` only
    /// when there are no keys at all.
    pub fn pick(&self) -> Option<KeyHandle> {
        self.pick_excluding(&[]).or_else(|| self.soonest_recovering())
    }

    /// A usable key whose index is not in `tried`; `None` when every usable
    /// key has been tried.
    pub fn pick_excluding(&self, tried: &[usize]) -> Option<KeyHandle> {
        if self.keys.is_empty() {
            return None;
        }
        let now = Instant::now();
        let benched = self.benched_until.lock().unwrap();
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        (0..self.keys.len())
            .map(|offset| (start + offset) % self.keys.len())
            .find(|i| !tried.contains(i) && benched[*i].is_none_or(|until| until <= now))
            .map(|index| KeyHandle {
                index,
                key: self.keys[index].clone(),
            })
    }

    fn soonest_recovering(&self) -> Option<KeyHandle> {
        let benched = self.benched_until.lock().unwrap();
        let (index, _) = benched
            .iter()
            .enumerate()
            .min_by_key(|(_, until)| until.unwrap_or_else(Instant::now))?;
        Some(KeyHandle {
            index,
            key: self.keys[index].clone(),
        })
    }

    /// Stop using key `index` for `duration`. A no-op for a single-key ring
    /// (see the module docs).
    pub fn bench(&self, index: usize, duration: Duration) {
        if self.keys.len() < 2 {
            return;
        }
        let until = Instant::now() + duration;
        let mut benched = self.benched_until.lock().unwrap();
        if let Some(slot) = benched.get_mut(index)
            && slot.is_none_or(|current| current < until)
        {
            *slot = Some(until);
        }
    }

    /// Time until the first benched key is usable again, if every key is
    /// benched right now; `None` when at least one key is usable.
    pub fn all_benched_for(&self) -> Option<Duration> {
        let now = Instant::now();
        let benched = self.benched_until.lock().unwrap();
        if benched.is_empty() {
            return None;
        }
        let mut soonest: Option<Duration> = None;
        for slot in benched.iter() {
            let until = (*slot)?;
            let remaining = until.checked_duration_since(now)?;
            soonest = Some(soonest.map_or(remaining, |s| s.min(remaining)));
        }
        soonest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(n: usize) -> KeyRing {
        KeyRing::new((0..n).map(|i| format!("key-{i}")).collect())
    }

    #[test]
    fn parse_splits_trims_and_drops_empties() {
        assert_eq!(KeyRing::parse("a"), vec!["a"]);
        assert_eq!(KeyRing::parse(" a , b,,c ,"), vec!["a", "b", "c"]);
        assert!(KeyRing::parse("").is_empty());
        assert!(KeyRing::parse(" , ").is_empty());
    }

    #[test]
    fn no_keys_means_no_pick() {
        let r = ring(0);
        assert!(r.is_empty());
        assert!(r.pick().is_none());
        assert!(r.pick_excluding(&[]).is_none());
        assert!(r.all_benched_for().is_none());
    }

    #[test]
    fn picks_rotate_round_robin() {
        let r = ring(3);
        let picked: Vec<usize> = (0..6).map(|_| r.pick().unwrap().index).collect();
        assert_eq!(picked, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn a_benched_key_is_skipped_until_it_recovers() {
        let r = ring(3);
        r.bench(1, Duration::from_millis(80));
        let picked: Vec<usize> = (0..4).map(|_| r.pick().unwrap().index).collect();
        assert!(!picked.contains(&1), "{picked:?}");
        std::thread::sleep(Duration::from_millis(100));
        let again: Vec<usize> = (0..3).map(|_| r.pick().unwrap().index).collect();
        assert!(again.contains(&1), "{again:?}");
    }

    #[test]
    fn pick_excluding_never_returns_a_tried_key_and_runs_out() {
        let r = ring(3);
        assert!(r.pick_excluding(&[0, 1]).unwrap().index == 2);
        assert!(r.pick_excluding(&[0, 1, 2]).is_none());
    }

    #[test]
    fn when_every_key_is_benched_the_soonest_one_is_still_picked() {
        let r = ring(2);
        r.bench(0, Duration::from_secs(300));
        r.bench(1, Duration::from_secs(5));
        assert_eq!(r.pick().unwrap().index, 1);
        let remaining = r.all_benched_for().unwrap();
        assert!(remaining <= Duration::from_secs(5), "{remaining:?}");
        assert!(r.pick_excluding(&[]).is_none(), "no usable key to rotate to");
    }

    #[test]
    fn a_lone_key_is_never_benched() {
        let r = ring(1);
        r.bench(0, Duration::from_secs(600));
        assert_eq!(r.pick().unwrap().index, 0);
        assert!(r.all_benched_for().is_none());
    }

    #[test]
    fn benching_never_shortens_an_existing_bench() {
        let r = ring(2);
        r.bench(0, Duration::from_secs(300));
        r.bench(0, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(r.pick_excluding(&[]).unwrap().index, 1);
        assert!(r.pick_excluding(&[1]).is_none(), "key 0 must still be benched");
    }
}
