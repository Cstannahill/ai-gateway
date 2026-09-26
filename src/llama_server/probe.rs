// ─── Probing a running llama-server ──────────────────────────────────────────
//
// Reads `/health`, `/props` and `/v1/models`. llama.cpp's response shapes vary
// between builds, so everything beyond "did it answer" is best-effort: a
// missing or renamed field yields `None`, never an error.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness as reported by the server's own `/health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// 200 — model loaded, accepting requests.
    Ok,
    /// 503 — the server is up but still loading the model.
    Loading,
    /// Answered with something else.
    Error,
}

/// What a live server says about itself.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ServerInfo {
    pub health: Option<Health>,
    /// Path of the loaded model file, as the server reports it.
    pub model_path: Option<String>,
    /// Model id from `/v1/models` (the `--alias`, or the file name).
    pub model_id: Option<String>,
    /// Context size the server is running with.
    pub n_ctx: Option<u64>,
    /// Context length the model was trained with.
    pub n_ctx_train: Option<u64>,
    pub n_params: Option<u64>,
    pub model_size_bytes: Option<u64>,
    pub vision: Option<bool>,
    pub audio: Option<bool>,
    pub build_info: Option<String>,
    pub total_slots: Option<u64>,
}

/// Just `/health` — cheap enough to poll while a model loads. `None` when
/// nothing is listening.
pub async fn health(client: &reqwest::Client, base_url: &str) -> Option<Health> {
    match client.get(format!("{base_url}/health")).timeout(PROBE_TIMEOUT).send().await {
        Ok(r) if r.status().is_success() => Some(Health::Ok),
        Ok(r) if r.status().as_u16() == 503 => Some(Health::Loading),
        Ok(_) => Some(Health::Error),
        Err(_) => None,
    }
}

/// `None` when nothing is listening (or it isn't answering HTTP in time).
pub async fn probe(client: &reqwest::Client, base_url: &str) -> Option<ServerInfo> {
    let health = match client.get(format!("{base_url}/health")).timeout(PROBE_TIMEOUT).send().await {
        Ok(r) if r.status().is_success() => Health::Ok,
        Ok(r) if r.status().as_u16() == 503 => Health::Loading,
        Ok(_) => Health::Error,
        Err(_) => return None,
    };
    let mut info = ServerInfo { health: Some(health), ..Default::default() };
    if health != Health::Ok {
        return Some(info);
    }

    let props_url = format!("{base_url}/props");
    let models_url = format!("{base_url}/v1/models");
    let (props, models) = tokio::join!(get_json(client, &props_url), get_json(client, &models_url));
    if let Some(p) = props {
        apply_props(&mut info, &p);
    }
    if let Some(m) = models {
        apply_models(&mut info, &m);
    }
    Some(info)
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let resp = client.get(url).timeout(PROBE_TIMEOUT).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

fn as_u64(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64)
}

pub(crate) fn apply_props(info: &mut ServerInfo, props: &Value) {
    info.model_path = props.get("model_path").and_then(Value::as_str).map(str::to_string);
    info.n_ctx = as_u64(props.pointer("/default_generation_settings/n_ctx"));
    info.total_slots = as_u64(props.get("total_slots"));
    info.build_info = props.get("build_info").and_then(Value::as_str).map(str::to_string);
    info.vision = props.pointer("/modalities/vision").and_then(Value::as_bool);
    info.audio = props.pointer("/modalities/audio").and_then(Value::as_bool);
}

pub(crate) fn apply_models(info: &mut ServerInfo, models: &Value) {
    let Some(first) = models.pointer("/data/0") else { return };
    info.model_id = first.get("id").and_then(Value::as_str).map(str::to_string);
    info.n_ctx_train = as_u64(first.pointer("/meta/n_ctx_train"));
    info.n_params = as_u64(first.pointer("/meta/n_params"));
    info.model_size_bytes = as_u64(first.pointer("/meta/size"));
}

/// Case- and separator-insensitive path comparison for matching a server's
/// reported `model_path` to a saved model.
pub fn same_path(a: &str, b: &str) -> bool {
    fn norm(p: &str) -> String {
        p.trim_start_matches(r"\\?\").replace('\\', "/").to_ascii_lowercase()
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_typical_props_and_models_payload() {
        let mut info = ServerInfo::default();
        apply_props(
            &mut info,
            &json!({
                "model_path": "S:\\models\\qwen.gguf",
                "total_slots": 1,
                "build_info": "b6000-abc",
                "modalities": {"vision": true, "audio": false},
                "default_generation_settings": {"n_ctx": 32768}
            }),
        );
        apply_models(
            &mut info,
            &json!({"data": [{"id": "qwen", "meta": {"n_ctx_train": 262144, "n_params": 9000000000u64, "size": 5000000000u64}}]}),
        );
        assert_eq!(info.model_path.as_deref(), Some("S:\\models\\qwen.gguf"));
        assert_eq!(info.n_ctx, Some(32768));
        assert_eq!(info.n_ctx_train, Some(262144));
        assert_eq!(info.total_slots, Some(1));
        assert_eq!(info.vision, Some(true));
        assert_eq!(info.audio, Some(false));
        assert_eq!(info.model_id.as_deref(), Some("qwen"));
        assert_eq!(info.model_size_bytes, Some(5_000_000_000));
    }

    #[test]
    fn missing_or_renamed_fields_yield_none_not_errors() {
        let mut info = ServerInfo::default();
        apply_props(&mut info, &json!({"something_else": 1}));
        apply_models(&mut info, &json!({"data": []}));
        apply_models(&mut info, &json!("not even an object"));
        assert!(info.model_path.is_none() && info.n_ctx.is_none() && info.vision.is_none() && info.model_id.is_none());
    }

    #[test]
    fn path_matching_ignores_case_separators_and_verbatim_prefix() {
        assert!(same_path(r"S:\Models\Qwen.gguf", "s:/models/qwen.gguf"));
        assert!(same_path(r"\\?\S:\m\a.gguf", r"S:\m\a.gguf"));
        assert!(!same_path(r"S:\m\a.gguf", r"S:\m\b.gguf"));
    }
}
