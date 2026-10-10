//! Environment variables as the gateway sees them, including `.env` changes
//! made after startup.
//!
//! At startup the env file is loaded into the process environment (real
//! variables win). A config reload re-reads that file, but mutating the
//! process environment once threads are running is unsound, so changes are
//! kept in an in-memory overlay instead and every provider-key lookup goes
//! through [`lookup`]:
//!
//! 1. a name the overlay knows → the overlay's value (`None` = removed from
//!    the file, so treated as unset even though the process environment still
//!    holds the value loaded at startup),
//! 2. otherwise the real process environment.
//!
//! Precedence is the startup rule: a variable that was set in the real
//! environment *before* the file was loaded is never overridden by the file.
//! Values are never logged — only names.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

fn overlay() -> &'static RwLock<HashMap<String, Option<String>>> {
    static OVERLAY: OnceLock<RwLock<HashMap<String, Option<String>>>> = OnceLock::new();
    OVERLAY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The value of an environment variable, honouring env-file changes applied by
/// [`EnvFile::reload`]. Use this instead of `std::env::var` for anything that
/// should follow a reload (provider API keys).
pub fn lookup(name: &str) -> Option<String> {
    if let Some(entry) = overlay().read().unwrap().get(name) {
        return entry.clone();
    }
    std::env::var(name).ok()
}

/// A saved copy of the overlay, to undo an env reload when a later step of a
/// config reload fails.
pub struct OverlaySnapshot(HashMap<String, Option<String>>);

pub fn snapshot() -> OverlaySnapshot {
    OverlaySnapshot(overlay().read().unwrap().clone())
}

pub fn restore(saved: OverlaySnapshot) {
    *overlay().write().unwrap() = saved.0;
}

/// What a reload of the env file changed. Names only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvChange {
    /// Variables whose effective value changed (added, edited or removed).
    pub changed: Vec<String>,
    /// Variables the file sets but a real environment variable overrides to a
    /// different value (a stale shell-profile key hiding the file's).
    pub shadowed: Vec<String>,
}

/// The env file the gateway loaded at startup, remembered so it can be
/// re-read.
#[derive(Debug, Clone)]
pub struct EnvFile {
    path: PathBuf,
    /// Names that the file (not the real environment) supplied at startup.
    /// They are in the process environment but must still be overridable by a
    /// newer file, unlike genuinely real variables.
    startup_applied: HashSet<String>,
}

impl EnvFile {
    pub fn new(path: PathBuf, startup_applied: impl IntoIterator<Item = String>) -> Self {
        Self {
            path,
            startup_applied: startup_applied.into_iter().collect(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the file and update the overlay. A missing file means "no
    /// variables": everything the file used to supply is removed. A file that
    /// exists but cannot be parsed is an error and changes nothing.
    pub fn reload(&self) -> Result<EnvChange, String> {
        let parsed: Vec<(String, String)> = if self.path.is_file() {
            dotenvy::from_path_iter(&self.path)
                .map_err(|e| format!("cannot read env file '{}': {e}", self.path.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("cannot parse env file '{}': {e}", self.path.display()))?
        } else {
            Vec::new()
        };
        Ok(self.apply(parsed))
    }

    /// Apply already-parsed file contents. Split out so the precedence rules
    /// can be tested without touching the filesystem or the real environment.
    fn apply(&self, file: Vec<(String, String)>) -> EnvChange {
        let mut change = EnvChange::default();
        let mut next: HashMap<String, Option<String>> = HashMap::new();
        let in_file: HashSet<&str> = file.iter().map(|(k, _)| k.as_str()).collect();

        for (name, value) in &file {
            let real = std::env::var(name).ok().filter(|_| !self.startup_applied.contains(name));
            match real {
                // A genuinely real variable always wins; the overlay stays out of it.
                Some(real_value) => {
                    if real_value != *value {
                        change.shadowed.push(name.clone());
                    }
                }
                None => {
                    next.insert(name.clone(), Some(value.clone()));
                }
            }
        }
        // Supplied by the file at startup, gone from it now: unset.
        for name in &self.startup_applied {
            if !in_file.contains(name.as_str()) {
                next.insert(name.clone(), None);
            }
        }

        let mut guard = overlay().write().unwrap();
        // Names in the old overlay that the new one no longer mentions go
        // back to whatever the real environment says.
        let mut touched: HashSet<String> = guard.keys().cloned().collect();
        touched.extend(next.keys().cloned());
        for name in touched {
            let before = match guard.get(&name) {
                Some(entry) => entry.clone(),
                None => std::env::var(&name).ok(),
            };
            let after = match next.get(&name) {
                Some(entry) => entry.clone(),
                None => std::env::var(&name).ok(),
            };
            if before != after {
                change.changed.push(name);
            }
        }
        *guard = next;
        drop(guard);

        change.changed.sort();
        change.shadowed.sort();
        change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The overlay is process-global and `apply` replaces it wholesale (as it
    // should in production, where there is one env file), so these tests take a
    // lock to run one at a time; they also use variable names nothing else touches.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn new_and_edited_values_are_visible_and_reported_by_name() {
        let _serial = serial();
        let env = EnvFile::new(PathBuf::from("x.env"), []);
        let change = env.apply(kv(&[("GW_TEST_ENV_A", "one")]));
        assert_eq!(lookup("GW_TEST_ENV_A").as_deref(), Some("one"));
        assert!(change.changed.contains(&"GW_TEST_ENV_A".to_string()));

        let change = env.apply(kv(&[("GW_TEST_ENV_A", "two")]));
        assert_eq!(lookup("GW_TEST_ENV_A").as_deref(), Some("two"));
        assert!(change.changed.contains(&"GW_TEST_ENV_A".to_string()));

        let change = env.apply(kv(&[("GW_TEST_ENV_A", "two")]));
        assert!(!change.changed.contains(&"GW_TEST_ENV_A".to_string()), "unchanged value is not a change");
    }

    #[test]
    fn a_variable_removed_from_the_file_becomes_unset_even_if_loaded_at_startup() {
        let _serial = serial();
        // Simulate startup having put the name in the process environment.
        // SAFETY: test-only, unique name, no other thread reads it.
        unsafe { std::env::set_var("GW_TEST_ENV_B", "from-startup-file") };
        let env = EnvFile::new(PathBuf::from("x.env"), ["GW_TEST_ENV_B".to_string()]);
        assert_eq!(lookup("GW_TEST_ENV_B").as_deref(), Some("from-startup-file"));

        let change = env.apply(vec![]);
        assert_eq!(lookup("GW_TEST_ENV_B"), None, "the stale startup value must not survive");
        assert!(change.changed.contains(&"GW_TEST_ENV_B".to_string()));
    }

    #[test]
    fn a_startup_file_value_can_be_overridden_by_a_newer_file() {
        let _serial = serial();
        // SAFETY: test-only, unique name.
        unsafe { std::env::set_var("GW_TEST_ENV_C", "old") };
        let env = EnvFile::new(PathBuf::from("x.env"), ["GW_TEST_ENV_C".to_string()]);
        env.apply(kv(&[("GW_TEST_ENV_C", "new")]));
        assert_eq!(lookup("GW_TEST_ENV_C").as_deref(), Some("new"));
    }

    #[test]
    fn a_real_environment_variable_always_wins_and_is_reported_as_shadowing() {
        let _serial = serial();
        // SAFETY: test-only, unique name.
        unsafe { std::env::set_var("GW_TEST_ENV_D", "real") };
        let env = EnvFile::new(PathBuf::from("x.env"), []); // NOT supplied by the file
        let change = env.apply(kv(&[("GW_TEST_ENV_D", "file")]));
        assert_eq!(lookup("GW_TEST_ENV_D").as_deref(), Some("real"));
        assert!(change.shadowed.contains(&"GW_TEST_ENV_D".to_string()));
        assert!(!change.changed.contains(&"GW_TEST_ENV_D".to_string()));

        // Same value in both places is not shadowing.
        let change = env.apply(kv(&[("GW_TEST_ENV_D", "real")]));
        assert!(!change.shadowed.contains(&"GW_TEST_ENV_D".to_string()));
    }

    #[test]
    fn reload_reads_the_file_and_a_missing_file_clears_what_it_supplied() {
        let _serial = serial();
        let dir = std::env::temp_dir().join(format!("gw-env-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "GW_TEST_ENV_E=\"k1,k2\"\n# comment\n").unwrap();

        let env = EnvFile::new(path.clone(), []);
        env.reload().unwrap();
        assert_eq!(lookup("GW_TEST_ENV_E").as_deref(), Some("k1,k2"));

        std::fs::write(&path, "GW_TEST_ENV_E=k1,k2,k3\n").unwrap();
        let change = env.reload().unwrap();
        assert_eq!(lookup("GW_TEST_ENV_E").as_deref(), Some("k1,k2,k3"));
        assert_eq!(change.changed, vec!["GW_TEST_ENV_E".to_string()]);

        std::fs::remove_file(&path).unwrap();
        env.reload().unwrap();
        // Not in startup_applied, so it falls back to the (unset) real environment.
        assert_eq!(lookup("GW_TEST_ENV_E"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_file_is_an_error_and_changes_nothing() {
        let _serial = serial();
        let dir = std::env::temp_dir().join(format!("gw-env-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "GW_TEST_ENV_F=keep\n").unwrap();
        let env = EnvFile::new(path.clone(), []);
        env.reload().unwrap();

        std::fs::write(&path, "this is = not = valid\nGW_TEST_ENV_F=\"unterminated\n").unwrap();
        assert!(env.reload().is_err());
        assert_eq!(lookup("GW_TEST_ENV_F").as_deref(), Some("keep"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
