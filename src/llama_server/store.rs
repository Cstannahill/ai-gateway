// ─── Saved model store ───────────────────────────────────────────────────────
//
// A small JSON file holding the user's saved models (alias -> gguf [+ mmproj]
// [+ per-model args]) and the global default args. Deliberately a file, not
// Postgres: this must keep working when the database is down, and it is a
// handful of human-edited records, not telemetry.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::{LlamaError, args};

const STORE_VERSION: u32 = 1;

/// One saved model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedModel {
    pub id: Uuid,
    /// Unique (case-insensitively) user-chosen name.
    pub alias: String,
    pub gguf_path: String,
    pub mmproj_path: Option<String>,
    /// Per-model argument string. `None` inherits the store's default args;
    /// `Some("")` means "explicitly no extra args".
    pub args: Option<String>,
    /// Pass `--alias <alias>` so llama-server's `/v1/models` reports it
    /// (skipped when the effective args already contain `-a/--alias`).
    #[serde(default = "default_true")]
    pub pass_alias: bool,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_true() -> bool {
    true
}

/// Create/update payload (`PUT` replaces every field; omitted optionals clear).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInput {
    pub alias: String,
    pub gguf_path: String,
    #[serde(default)]
    pub mmproj_path: Option<String>,
    #[serde(default)]
    pub args: Option<String>,
    #[serde(default)]
    pub pass_alias: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    default_args: String,
    models: Vec<SavedModel>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self { version: STORE_VERSION, default_args: String::new(), models: vec![] }
    }
}

pub struct ModelStore {
    path: PathBuf,
    inner: RwLock<StoreFile>,
}

/// Validate one model-file path: absolute, exists, is a file, `.gguf`.
/// Returns the canonical path without Windows' `\\?\` verbatim prefix.
pub fn validate_model_file(raw: &str, what: &str) -> Result<String, LlamaError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LlamaError::invalid("invalid_path", format!("{what} path is empty")));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(LlamaError::invalid(
            "invalid_path",
            format!("{what} path must be absolute (got '{raw}')"),
        ));
    }
    let canonical = path.canonicalize().map_err(|_| {
        LlamaError::invalid("path_not_found", format!("{what} file '{raw}' does not exist"))
    })?;
    if !canonical.is_file() {
        return Err(LlamaError::invalid("path_not_file", format!("{what} path '{raw}' is not a file")));
    }
    let is_gguf = canonical
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"));
    if !is_gguf {
        return Err(LlamaError::invalid(
            "not_gguf",
            format!("{what} file '{raw}' must have a .gguf extension"),
        ));
    }
    Ok(strip_verbatim(&canonical))
}

fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// Canonical form of an existing path (no Windows `\\?\` prefix), or `None`
/// if it can't be resolved. Saved model paths are stored in this form, so
/// anything compared against them must be normalized the same way.
pub fn canonical_string(p: &Path) -> Option<String> {
    p.canonicalize().ok().map(|c| strip_verbatim(&c))
}

pub fn validate_alias(alias: &str) -> Result<(), LlamaError> {
    let ok = !alias.is_empty()
        && alias.len() <= 64
        && alias.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(LlamaError::invalid(
            "invalid_alias",
            "alias must be 1-64 characters of letters, digits, '.', '_' or '-'".to_string(),
        ))
    }
}

/// Reject an args string with syntax errors or reserved flags.
pub fn validate_args(raw: &str) -> Result<(), LlamaError> {
    let parsed = args::parse_and_validate(raw);
    match parsed.issues.first() {
        None => Ok(()),
        Some(issue) => Err(LlamaError::invalid(
            if issue.code == "reserved_flag" { "reserved_flag" } else { "invalid_args" },
            issue.message.clone(),
        )),
    }
}

impl ModelStore {
    /// Load the store, creating an empty one if the file doesn't exist. An
    /// unreadable/corrupt file is an error and is left untouched — silently
    /// replacing a hand-edited file would destroy the user's saved models.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, LlamaError> {
        let path = path.into();
        let file = match tokio::fs::read_to_string(&path).await {
            Ok(text) => serde_json::from_str::<StoreFile>(&text).map_err(|e| {
                LlamaError::Internal(format!(
                    "model store '{}' is not valid JSON ({e}); fix or remove it (it was not modified)",
                    path.display()
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreFile::default(),
            Err(e) => {
                return Err(LlamaError::Internal(format!("cannot read model store '{}': {e}", path.display())));
            }
        };
        Ok(Self { path, inner: RwLock::new(file) })
    }

    pub async fn list(&self) -> Vec<SavedModel> {
        self.inner.read().await.models.clone()
    }

    pub async fn get(&self, id: Uuid) -> Option<SavedModel> {
        self.inner.read().await.models.iter().find(|m| m.id == id).cloned()
    }

    pub async fn default_args(&self) -> String {
        self.inner.read().await.default_args.clone()
    }

    pub async fn set_default_args(&self, raw: String) -> Result<String, LlamaError> {
        validate_args(&raw)?;
        let mut guard = self.inner.write().await;
        let previous = std::mem::replace(&mut guard.default_args, raw.clone());
        if let Err(e) = self.persist(&guard).await {
            guard.default_args = previous;
            return Err(e);
        }
        Ok(raw)
    }

    /// Validate everything about `input` (paths, alias, args) without
    /// touching the store — also used by the validate endpoint.
    pub fn validate_input(input: &ModelInput) -> Result<(String, Option<String>), LlamaError> {
        validate_alias(&input.alias)?;
        let gguf = validate_model_file(&input.gguf_path, "model")?;
        let mmproj = match input.mmproj_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => Some(validate_model_file(p, "mmproj")?),
            None => None,
        };
        if let Some(a) = &input.args {
            validate_args(a)?;
        }
        Ok((gguf, mmproj))
    }

    pub async fn create(&self, input: ModelInput) -> Result<SavedModel, LlamaError> {
        let (gguf, mmproj) = Self::validate_input(&input)?;
        let mut guard = self.inner.write().await;
        if guard.models.iter().any(|m| m.alias.eq_ignore_ascii_case(&input.alias)) {
            return Err(LlamaError::conflict(
                "alias_taken",
                format!("a saved model named '{}' already exists", input.alias),
            ));
        }
        let now = Utc::now();
        let model = SavedModel {
            id: Uuid::now_v7(),
            alias: input.alias,
            gguf_path: gguf,
            mmproj_path: mmproj,
            args: input.args,
            pass_alias: input.pass_alias.unwrap_or(true),
            notes: input.notes.filter(|n| !n.trim().is_empty()),
            created_at: now,
            updated_at: now,
        };
        guard.models.push(model.clone());
        if let Err(e) = self.persist(&guard).await {
            guard.models.pop();
            return Err(e);
        }
        Ok(model)
    }

    pub async fn update(&self, id: Uuid, input: ModelInput) -> Result<SavedModel, LlamaError> {
        let (gguf, mmproj) = Self::validate_input(&input)?;
        let mut guard = self.inner.write().await;
        if guard
            .models
            .iter()
            .any(|m| m.id != id && m.alias.eq_ignore_ascii_case(&input.alias))
        {
            return Err(LlamaError::conflict(
                "alias_taken",
                format!("a saved model named '{}' already exists", input.alias),
            ));
        }
        let Some(idx) = guard.models.iter().position(|m| m.id == id) else {
            return Err(LlamaError::NotFound(format!("no saved model with id {id}")));
        };
        let previous = guard.models[idx].clone();
        let updated = SavedModel {
            id,
            alias: input.alias,
            gguf_path: gguf,
            mmproj_path: mmproj,
            args: input.args,
            pass_alias: input.pass_alias.unwrap_or(true),
            notes: input.notes.filter(|n| !n.trim().is_empty()),
            created_at: previous.created_at,
            updated_at: Utc::now(),
        };
        guard.models[idx] = updated.clone();
        if let Err(e) = self.persist(&guard).await {
            guard.models[idx] = previous;
            return Err(e);
        }
        Ok(updated)
    }

    pub async fn delete(&self, id: Uuid) -> Result<SavedModel, LlamaError> {
        let mut guard = self.inner.write().await;
        let Some(idx) = guard.models.iter().position(|m| m.id == id) else {
            return Err(LlamaError::NotFound(format!("no saved model with id {id}")));
        };
        let removed = guard.models.remove(idx);
        if let Err(e) = self.persist(&guard).await {
            guard.models.insert(idx, removed);
            return Err(e);
        }
        Ok(removed)
    }

    /// Atomic write: temp file in the same directory, then rename over the
    /// original, so a crash mid-write can't leave a truncated store.
    async fn persist(&self, file: &StoreFile) -> Result<(), LlamaError> {
        let json = serde_json::to_string_pretty(file)
            .map_err(|e| LlamaError::Internal(format!("cannot serialize model store: {e}")))?;
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| LlamaError::Internal(format!("cannot create '{}': {e}", parent.display())))?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|e| LlamaError::Internal(format!("cannot write '{}': {e}", tmp.display())))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| LlamaError::Internal(format!("cannot replace '{}': {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch dir with a couple of fake model files.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ai_gateway_store_{}", Uuid::now_v7()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self, name: &str) -> String {
            let p = self.0.join(name);
            std::fs::write(&p, b"x").unwrap();
            p.to_string_lossy().into_owned()
        }
        fn store_path(&self) -> PathBuf {
            self.0.join("models.json")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn input(alias: &str, gguf: &str) -> ModelInput {
        ModelInput { alias: alias.into(), gguf_path: gguf.into(), mmproj_path: None, args: None, pass_alias: None, notes: None }
    }

    #[tokio::test]
    async fn create_list_update_delete_round_trip_and_persist() {
        let s = Scratch::new();
        let a = s.file("a.gguf");
        let b = s.file("b.gguf");
        let proj = s.file("mmproj-a.gguf");

        let store = ModelStore::open(s.store_path()).await.unwrap();
        assert!(store.list().await.is_empty());

        let mut inp = input("qwen-vl", &a);
        inp.mmproj_path = Some(proj.clone());
        inp.args = Some("-ngl all -fa on".into());
        let created = store.create(inp).await.unwrap();
        assert_eq!(created.args.as_deref(), Some("-ngl all -fa on"));
        assert!(created.mmproj_path.is_some());
        assert!(created.pass_alias, "pass_alias defaults to true");

        // A fresh store instance sees the persisted data.
        let reopened = ModelStore::open(s.store_path()).await.unwrap();
        assert_eq!(reopened.list().await, vec![created.clone()]);

        let updated = reopened.update(created.id, input("qwen-vl-2", &b)).await.unwrap();
        assert_eq!(updated.alias, "qwen-vl-2");
        assert!(updated.mmproj_path.is_none(), "PUT replaces: omitted optionals clear");
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);

        reopened.delete(created.id).await.unwrap();
        assert!(ModelStore::open(s.store_path()).await.unwrap().list().await.is_empty());
        assert!(matches!(reopened.delete(created.id).await, Err(LlamaError::NotFound(_))));
    }

    #[tokio::test]
    async fn alias_uniqueness_is_case_insensitive_and_allows_self_update() {
        let s = Scratch::new();
        let a = s.file("a.gguf");
        let b = s.file("b.gguf");
        let store = ModelStore::open(s.store_path()).await.unwrap();
        let first = store.create(input("Llama", &a)).await.unwrap();
        let err = store.create(input("llama", &b)).await.unwrap_err();
        assert_eq!(err.code(), "alias_taken");
        // Re-saving the same model under its own alias is fine.
        assert!(store.update(first.id, input("Llama", &b)).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_bad_aliases_paths_and_args() {
        let s = Scratch::new();
        let good = s.file("a.gguf");
        let txt = s.file("notes.txt");
        let store = ModelStore::open(s.store_path()).await.unwrap();

        for bad in ["", "has space", "slash/name", &"x".repeat(65)] {
            assert_eq!(store.create(input(bad, &good)).await.unwrap_err().code(), "invalid_alias", "{bad:?}");
        }
        assert_eq!(store.create(input("a", "relative/x.gguf")).await.unwrap_err().code(), "invalid_path");
        assert_eq!(store.create(input("a", &s.0.join("missing.gguf").to_string_lossy())).await.unwrap_err().code(), "path_not_found");
        assert_eq!(store.create(input("a", &txt)).await.unwrap_err().code(), "not_gguf");
        assert_eq!(store.create(input("a", &s.0.to_string_lossy())).await.unwrap_err().code(), "path_not_file");

        let mut with_bad_mmproj = input("a", &good);
        with_bad_mmproj.mmproj_path = Some(txt.clone());
        assert_eq!(store.create(with_bad_mmproj).await.unwrap_err().code(), "not_gguf");

        let mut reserved = input("a", &good);
        reserved.args = Some("-ngl 1 --host 0.0.0.0".into());
        assert_eq!(store.create(reserved).await.unwrap_err().code(), "reserved_flag");
        let mut unbalanced = input("a", &good);
        unbalanced.args = Some("--x 'oops".into());
        assert_eq!(store.create(unbalanced).await.unwrap_err().code(), "invalid_args");

        assert!(store.list().await.is_empty(), "failed creates must not leave anything behind");
    }

    #[tokio::test]
    async fn default_args_are_validated_and_persisted() {
        let s = Scratch::new();
        let store = ModelStore::open(s.store_path()).await.unwrap();
        assert_eq!(store.default_args().await, "");
        assert_eq!(store.set_default_args("--port 1".into()).await.unwrap_err().code(), "reserved_flag");
        assert_eq!(store.default_args().await, "", "a rejected value must not be stored");
        store.set_default_args("-ngl all -fa on".into()).await.unwrap();
        assert_eq!(ModelStore::open(s.store_path()).await.unwrap().default_args().await, "-ngl all -fa on");
    }

    #[tokio::test]
    async fn corrupt_store_file_is_reported_and_left_untouched() {
        let s = Scratch::new();
        std::fs::write(s.store_path(), "{ this is not json").unwrap();
        let err = ModelStore::open(s.store_path()).await.err().expect("must fail");
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        assert_eq!(std::fs::read_to_string(s.store_path()).unwrap(), "{ this is not json");
    }

    #[tokio::test]
    async fn stored_paths_have_no_windows_verbatim_prefix() {
        let s = Scratch::new();
        let a = s.file("a.gguf");
        let store = ModelStore::open(s.store_path()).await.unwrap();
        let m = store.create(input("a", &a)).await.unwrap();
        assert!(!m.gguf_path.starts_with(r"\\?\"), "{}", m.gguf_path);
    }
}
