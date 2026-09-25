//! Opt-in live smoke tests: one tiny non-streaming chat request and one
//! streaming chat request per provider, sent through the same adapter
//! construction path `main.rs` uses (`providers::build_adapter`).
//!
//! Every other test in this repository uses mock providers, so none of them
//! can catch a vendor quirk (auth header shape, URL layout, SSE framing,
//! usage reporting). These can — but only for providers you actually
//! configure. Each test is a silent no-op (it prints a `SKIP` line) unless
//! its trigger environment variable is set, so `cargo test` stays green and
//! free with no setup.
//!
//! Run with output visible, e.g.:
//!
//!   LIVE_LLAMA_BASE_URL=http://localhost:8080 \
//!   OPENROUTER_API_KEY=sk-or-... \
//!   cargo test --test live_provider_smoke_tests -- --nocapture --test-threads=1
//!
//! Environment (per provider; "required" ones skip with a message if absent):
//!
//!   llama_cpp    LIVE_LLAMA_BASE_URL (trigger)   LIVE_LLAMA_MODEL (default "local")
//!   ollama       LIVE_OLLAMA_BASE_URL (trigger)  LIVE_OLLAMA_MODEL (required)
//!   vllm         LIVE_VLLM_BASE_URL (trigger)    LIVE_VLLM_MODEL (required)
//!   openrouter   OPENROUTER_API_KEY (trigger)    LIVE_OPENROUTER_MODEL (default openai/gpt-4o-mini)
//!   gemini       GEMINI_API_KEY (trigger)        LIVE_GEMINI_MODEL (default gemini-2.0-flash)
//!   hugging_face HF_API_KEY (trigger)            LIVE_HF_MODEL (required)
//!   ollama_cloud OLLAMA_API_KEY (trigger)        LIVE_OLLAMA_CLOUD_MODEL (required),
//!                                                LIVE_OLLAMA_CLOUD_BASE_URL (default https://ollama.com)
//!   generic      LIVE_GENERIC_BASE_URL (trigger) LIVE_GENERIC_MODEL (required),
//!                LIVE_GENERIC_API_KEY (optional) — any OpenAI-compatible endpoint,
//!                which is also how xai/venice/featherless are served today.
//!
//! Cloud targets spend a few tokens (max_tokens is capped at 64), i.e. a
//! fraction of a cent per run.

use std::time::Duration;

use ai_gateway::providers::events::ProviderEvent;
use ai_gateway::providers::{self, ProviderOutput};
use ai_gateway::types::{GatewayRequest, Message, MessageContent, ProviderConfig, Role};
use futures::StreamExt;
use serde_json::json;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

struct Target {
    /// Label used in output and as the provider's configured name.
    label: &'static str,
    /// Serde name of `ProviderType`.
    provider_type: &'static str,
    /// Env var that opts this provider in. Unset → skip.
    trigger_env: &'static str,
    /// True when the trigger env var holds an API key rather than a base URL.
    trigger_is_key: bool,
    base_url_env: Option<&'static str>,
    default_base_url: Option<&'static str>,
    key_env: Option<&'static str>,
    model_env: &'static str,
    default_model: Option<&'static str>,
}

impl Target {
    /// Build the provider config from the environment, or explain why this
    /// target is being skipped.
    fn resolve(&self) -> Result<(ProviderConfig, String), String> {
        let trigger = std::env::var(self.trigger_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{} not set", self.trigger_env))?;

        let base_url = if self.trigger_is_key {
            self.base_url_env
                .and_then(|e| std::env::var(e).ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| self.default_base_url.map(str::to_string))
                .ok_or("no base URL available")?
        } else {
            trigger.clone()
        };

        let api_key = if self.trigger_is_key {
            Some(trigger)
        } else {
            self.key_env
                .and_then(|e| std::env::var(e).ok())
                .filter(|v| !v.trim().is_empty())
        };

        let model = std::env::var(self.model_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.default_model.map(str::to_string))
            .ok_or_else(|| format!("{} is required for this provider", self.model_env))?;

        let mut cfg = json!({
            "name": self.label,
            "type": self.provider_type,
            "base_url": base_url,
            "timeout_secs": 60,
        });
        if let Some(key) = api_key {
            cfg["api_key"] = json!(key);
        }
        let config: ProviderConfig = serde_json::from_value(cfg)
            .map_err(|e| format!("could not build ProviderConfig: {e}"))?;
        Ok((config, model))
    }
}

fn request(model: &str, stream: bool) -> GatewayRequest {
    GatewayRequest {
        model: model.to_string(),
        messages: Some(vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Reply with the single word: pong".to_string())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }]),
        temperature: Some(0.0),
        max_tokens: Some(64),
        stream,
        ..Default::default()
    }
}

/// Everything worth asserting about one request's event sequence.
struct Observed {
    text_chars: usize,
    reasoning_chars: usize,
    content_deltas: usize,
    completed: bool,
    finish_reason: Option<String>,
    total_tokens: Option<u64>,
    errors: Vec<String>,
}

async fn run_one(
    provider: &dyn providers::Provider,
    model: &str,
    stream: bool,
) -> Result<Observed, String> {
    let req = request(model, stream);
    let started = std::time::Instant::now();

    let events: Vec<ProviderEvent> = tokio::time::timeout(REQUEST_TIMEOUT, async {
        match provider.execute(&req, model).await {
            Err(e) => Err(format!("execute() failed: {e:#}")),
            Ok(ProviderOutput::Complete(ev)) => Ok(vec![ev]),
            Ok(ProviderOutput::Stream(s)) => Ok(s.collect::<Vec<_>>().await),
        }
    })
    .await
    .map_err(|_| format!("timed out after {REQUEST_TIMEOUT:?}"))??;

    let mut o = Observed {
        text_chars: 0,
        reasoning_chars: 0,
        content_deltas: 0,
        completed: false,
        finish_reason: None,
        total_tokens: None,
        errors: vec![],
    };
    for ev in &events {
        match ev {
            ProviderEvent::ContentDelta { delta, .. } => {
                o.content_deltas += 1;
                o.text_chars += delta.len();
            }
            ProviderEvent::ReasoningDelta { delta, .. } => o.reasoning_chars += delta.len(),
            ProviderEvent::Completed { finish_reason, .. } => {
                o.completed = true;
                o.finish_reason = finish_reason.clone();
            }
            ProviderEvent::UsageUpdate { total_tokens, .. } => o.total_tokens = *total_tokens,
            ProviderEvent::Error { code, message, status_code, .. } => {
                o.errors.push(format!("[{code}] status={status_code:?}: {message}"));
            }
            _ => {}
        }
    }
    eprintln!(
        "    {} done in {:?}: {} event(s), {} content chars over {} delta(s), {} reasoning chars, finish={:?}, total_tokens={:?}",
        if stream { "stream    " } else { "non-stream" },
        started.elapsed(),
        events.len(),
        o.text_chars,
        o.content_deltas,
        o.reasoning_chars,
        o.finish_reason,
        o.total_tokens,
    );
    Ok(o)
}

fn check(mode: &str, o: &Observed) -> Vec<String> {
    let mut failures = vec![];
    if !o.errors.is_empty() {
        failures.push(format!("{mode}: provider reported error(s): {}", o.errors.join("; ")));
    }
    if !o.completed {
        failures.push(format!("{mode}: no Completed event (stream ended without a terminal event)"));
    }
    // A reasoning model can spend the whole 64-token budget thinking, so
    // reasoning output counts as a live response too.
    if o.text_chars + o.reasoning_chars == 0 {
        failures.push(format!("{mode}: response contained no content or reasoning text"));
    }
    failures
}

async fn smoke(target: Target) {
    let (config, model) = match target.resolve() {
        Ok(v) => v,
        Err(why) => {
            eprintln!("SKIP {:<13} ({why})", target.label);
            return;
        }
    };
    eprintln!("LIVE {:<13} {} model={model}", target.label, config.base_url);

    let provider = providers::build_adapter(&config).expect("adapter should construct from config");
    eprintln!("    health: {:?} (informational)", provider.health().await);

    let mut failures = vec![];
    for (mode, stream) in [("non-streaming", false), ("streaming", true)] {
        match run_one(provider.as_ref(), &model, stream).await {
            Ok(o) => {
                failures.extend(check(mode, &o));
                if stream && o.content_deltas + usize::from(o.reasoning_chars > 0) == 0 {
                    failures.push("streaming: no incremental deltas arrived".to_string());
                }
            }
            Err(e) => failures.push(format!("{mode}: {e}")),
        }
    }

    assert!(
        failures.is_empty(),
        "live smoke test for '{}' failed:\n  - {}",
        target.label,
        failures.join("\n  - ")
    );
}

#[tokio::test]
async fn live_llama_cpp() {
    smoke(Target {
        label: "llama_cpp",
        provider_type: "llama_cpp",
        trigger_env: "LIVE_LLAMA_BASE_URL",
        trigger_is_key: false,
        base_url_env: None,
        default_base_url: None,
        key_env: None,
        model_env: "LIVE_LLAMA_MODEL",
        default_model: Some("local"),
    })
    .await;
}

#[tokio::test]
async fn live_ollama() {
    smoke(Target {
        label: "ollama",
        provider_type: "ollama",
        trigger_env: "LIVE_OLLAMA_BASE_URL",
        trigger_is_key: false,
        base_url_env: None,
        default_base_url: None,
        key_env: None,
        model_env: "LIVE_OLLAMA_MODEL",
        default_model: None,
    })
    .await;
}

#[tokio::test]
async fn live_vllm() {
    smoke(Target {
        label: "vllm",
        provider_type: "vllm",
        trigger_env: "LIVE_VLLM_BASE_URL",
        trigger_is_key: false,
        base_url_env: None,
        default_base_url: None,
        key_env: None,
        model_env: "LIVE_VLLM_MODEL",
        default_model: None,
    })
    .await;
}

#[tokio::test]
async fn live_openrouter() {
    smoke(Target {
        label: "openrouter",
        provider_type: "open_router",
        trigger_env: "OPENROUTER_API_KEY",
        trigger_is_key: true,
        base_url_env: None,
        default_base_url: Some("https://openrouter.ai/api"),
        key_env: None,
        model_env: "LIVE_OPENROUTER_MODEL",
        default_model: Some("openai/gpt-4o-mini"),
    })
    .await;
}

#[tokio::test]
async fn live_gemini() {
    smoke(Target {
        label: "gemini",
        provider_type: "gemini",
        trigger_env: "GEMINI_API_KEY",
        trigger_is_key: true,
        base_url_env: None,
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        key_env: None,
        model_env: "LIVE_GEMINI_MODEL",
        default_model: Some("gemini-2.0-flash"),
    })
    .await;
}

#[tokio::test]
async fn live_hugging_face() {
    smoke(Target {
        label: "hugging_face",
        provider_type: "hugging_face",
        trigger_env: "HF_API_KEY",
        trigger_is_key: true,
        base_url_env: None,
        default_base_url: Some("https://router.huggingface.co/v1"),
        key_env: None,
        model_env: "LIVE_HF_MODEL",
        default_model: None,
    })
    .await;
}

#[tokio::test]
async fn live_ollama_cloud() {
    smoke(Target {
        label: "ollama_cloud",
        provider_type: "ollama_cloud",
        trigger_env: "OLLAMA_API_KEY",
        trigger_is_key: true,
        base_url_env: Some("LIVE_OLLAMA_CLOUD_BASE_URL"),
        default_base_url: Some("https://ollama.com"),
        key_env: None,
        model_env: "LIVE_OLLAMA_CLOUD_MODEL",
        default_model: None,
    })
    .await;
}

#[tokio::test]
async fn live_generic_openai_compatible() {
    smoke(Target {
        label: "generic",
        provider_type: "open_ai_compatible",
        trigger_env: "LIVE_GENERIC_BASE_URL",
        trigger_is_key: false,
        base_url_env: None,
        default_base_url: None,
        key_env: Some("LIVE_GENERIC_API_KEY"),
        model_env: "LIVE_GENERIC_MODEL",
        default_model: None,
    })
    .await;
}

/// Guards the harness itself: with no trigger variables set, every target
/// must resolve to a skip (never a failure), so the default `cargo test`
/// run can't be broken or made to spend money by this file.
#[test]
fn targets_skip_cleanly_when_not_configured() {
    let t = Target {
        label: "unconfigured",
        provider_type: "open_ai_compatible",
        trigger_env: "LIVE_SMOKE_TEST_DEFINITELY_UNSET_VAR",
        trigger_is_key: false,
        base_url_env: None,
        default_base_url: None,
        key_env: None,
        model_env: "LIVE_SMOKE_TEST_DEFINITELY_UNSET_MODEL",
        default_model: None,
    };
    assert!(t.resolve().is_err());
}
