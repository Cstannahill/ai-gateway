use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream;
use reqwest::Client;
use url::Url;

use super::{Provider, ProviderOutput};
use super::events::ProviderEvent;
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

// ─── OpenAI-Compatible Adapter ───────────────────────────────────────────────
//
// Generic adapter for any provider that exposes OpenAI-compatible endpoints.
// Covers: llama-server, vLLM, OpenRouter, HuggingFace, and others.
//
// Provider-specific adapters extend this with:
// - Custom header injection (API keys, provider metadata)
// - Provider-specific request parameters (min_p, top_k, etc.)
// - Response enrichment (upstream_provider extraction, GPU metrics)
// - Capability overrides

/// True if a URL path segment looks like an API version marker, e.g.
/// `v1`, `v1beta`, `v2alpha` — a `v` followed by at least one digit.
fn is_version_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    if chars.next() != Some('v') {
        return false;
    }
    chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// Configuration for an OpenAI-compatible provider instance.
pub struct OpenAICompatibleAdapter {
    /// Provider name (from config).
    name: String,
    /// Base URL for the API (e.g. "http://localhost:8080/v1").
    base_url: Url,
    /// HTTP client (shared, connection-pooled).
    client: Client,
    /// API key (resolved from env or config).
    api_key: Option<String>,
    /// Default headers for every request.
    default_headers: HashMap<String, String>,
    /// Known capabilities.
    capabilities: ModelCapabilities,
    /// Request timeout.
    #[allow(dead_code)]
    timeout: std::time::Duration,
}

impl OpenAICompatibleAdapter {
    /// Create a new adapter from provider configuration.
    pub fn new(config: &ProviderConfig, capabilities: ModelCapabilities) -> anyhow::Result<Self> {
        // Resolve API key from env var or direct config.
        let api_key = config
            .api_key.clone()
            .or_else(|| {
                config
                    .api_key_env
                    .as_ref()
                    .and_then(|env| std::env::var(env).ok())
            });

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .pool_max_idle_per_host(10)
            .build()?;

        Ok(Self {
            name: config.name.clone(),
            base_url: config.base_url.clone(),
            client,
            api_key,
            default_headers: config.default_headers.clone().unwrap_or_default(),
            capabilities,
            timeout: std::time::Duration::from_secs(config.timeout_secs),
        })
    }

    /// Build the full URL for a given path (e.g. "/v1/chat/completions").
    ///
    /// Providers configure their `base_url` inconsistently with respect to
    /// API versioning: some are unversioned roots that need the canonical
    /// `/v1/...` path appended in full (llama.cpp, OpenRouter), while others
    /// already embed a version segment somewhere in their path that is not
    /// literally `v1` (e.g. Gemini's OpenAI-compat root is
    /// `.../v1beta/openai`). Appending our own `v1/` on top of an
    /// already-versioned base produces a broken URL (previously this only
    /// checked for a literal `/v1` suffix, which missed `v1beta` and any
    /// non-trailing version segment). We instead check whether *any* path
    /// segment of the base URL looks like an API version marker
    /// (`v` followed by a digit, e.g. `v1`, `v1beta`, `v2alpha`) and skip
    /// re-adding `v1/` if so.
    pub fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        let path_clean = path.strip_prefix('/').unwrap_or(path);

        let base_already_versioned = self
            .base_url
            .path_segments()
            .map(|mut segments| segments.any(is_version_segment))
            .unwrap_or(false);

        if base_already_versioned && path_clean.starts_with("v1/") {
            format!("{}/{}", base, &path_clean[3..])
        } else {
            format!("{}/{}", base, path_clean)
        }
    }

    /// Build common headers (Authorization, Content-Type, etc.).
    ///
    /// Returns an error rather than panicking if `api_key` contains bytes
    /// that aren't a valid HTTP header value (e.g. a stray newline from a
    /// copy-pasted key) — this runs on *every* request to this provider, so
    /// a `.unwrap()` here would panic every single request until the
    /// operator fixed their config, rather than failing that one request
    /// with a clear message.
    fn build_headers(&self) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        if let Some(ref key) = self.api_key {
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key))
                .map_err(|e| anyhow::anyhow!("provider '{}' has an invalid api_key (not a valid HTTP header value): {e}", self.name))?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }

        for (k, v) in &self.default_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        }

        Ok(headers)
    }

    /// Build the request body for a chat completion.
    /// Subclasses can override this to inject provider-specific params.
    pub fn build_chat_body(
        &self,
        request: &GatewayRequest,
        model: &str,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": model,
            "stream": request.stream,
        });

        if let Some(ref messages) = request.messages {
            body["messages"] = serde_json::to_value(messages).unwrap_or_default();
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(max) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max);
        }
        if let Some(max_completion) = request.max_completion_tokens {
            // OpenAI's o-series models reject `max_tokens` and require this
            // instead; previously silently dropped (Audit A08).
            body["max_completion_tokens"] = serde_json::json!(max_completion);
        }
        if let Some(ref stop) = request.stop {
            body["stop"] = serde_json::to_value(stop).unwrap_or_default();
        }
        if let Some(ref tools) = request.tools {
            body["tools"] = serde_json::to_value(tools).unwrap_or_default();
        }
        if let Some(ref tool_choice) = request.tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }
        if let Some(parallel) = request.parallel_tool_calls {
            body["parallel_tool_calls"] = serde_json::json!(parallel);
        }
        if let Some(ref modalities) = request.modalities {
            // Previously accepted by the request schema but never
            // forwarded to any provider (Audit A08) — accept-and-ignore is
            // worse than either forwarding or rejecting, so it's forwarded
            // as-is; providers that don't support a requested modality will
            // reject it themselves rather than the gateway silently
            // pretending it was honored.
            body["modalities"] = serde_json::to_value(modalities).unwrap_or_default();
        }
        // Top-level `reasoning_effort` (OpenAI o-series) is independent of
        // the nested `reasoning` object below (OpenRouter/Anthropic-style);
        // a caller may set either or both. Previously silently dropped
        // (Audit A08).
        if let Some(ref effort) = request.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(effort);
        }
        if let Some(ref reasoning) = request.reasoning
            && reasoning.enabled
        {
            // The canonical `reasoning` control participated in capability
            // filtering but was never actually forwarded to any provider
            // (Audit A07). There is no single universal wire format for
            // this across OpenAI-compatible providers, so we emit both of
            // the two shapes in common use and let providers ignore the one
            // they don't recognize: OpenRouter/Anthropic-style providers
            // read a nested `reasoning` object, while OpenAI's o-series
            // models read a top-level `reasoning_effort` string.
            let mut reasoning_obj = serde_json::Map::new();
            reasoning_obj.insert("enabled".to_string(), serde_json::json!(true));
            if let Some(effort) = &reasoning.effort {
                reasoning_obj.insert("effort".to_string(), serde_json::json!(effort));
                body["reasoning_effort"] = serde_json::json!(effort);
            }
            if let Some(max_tokens) = reasoning.max_tokens {
                reasoning_obj.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
            }
            body["reasoning"] = serde_json::Value::Object(reasoning_obj);
        }
        if let Some(ref format) = request.response_format {
            body["response_format"] = serde_json::to_value(format).unwrap_or_default();
        }
        if let Some(n) = request.n {
            body["n"] = serde_json::json!(n);
        }
        if let Some(fp) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(fp);
        }
        if let Some(pp) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pp);
        }
        if let Some(seed) = request.seed {
            body["seed"] = serde_json::json!(seed);
        }
        if let Some(ref user) = request.user {
            body["user"] = serde_json::json!(user);
        }

        // ─── Provider options escape hatch ──────────────────────────────
        if let Some(ref opts) = request.provider_options {
            for (key, val) in opts {
                if (key == &self.name || key == "openai" || key == "openai_compatible")
                    && let Some(obj) = val.as_object()
                {
                    for (param_k, param_v) in obj {
                        body[param_k] = param_v.clone();
                    }
                }
            }
        }

        body
    }

    /// Convert a complete non-streaming chat-completions response body into
    /// the internal `ProviderEvent` sequence, one content/tool-call set per
    /// choice — not just `choices[0]` — so an `n > 1` request's other
    /// choices actually reach the client and telemetry instead of being
    /// silently dropped (docs/REMEDIATION_PLAN.md Phase 2.3 / Audit A08).
    /// Reasoning is only captured for choice 0: `ReasoningDelta` has no
    /// per-choice index in the event model, and reasoning-capable models
    /// are not commonly combined with `n > 1` — documented scoping, not an
    /// oversight. Factored out from `execute()` so it can be unit tested
    /// without a network round-trip.
    pub fn events_from_non_streaming_response(
        response_body: &serde_json::Value,
        started: DateTime<Utc>,
        provider_name: String,
        model_name: String,
    ) -> Vec<ProviderEvent> {
        let choices = response_body["choices"].as_array().cloned().unwrap_or_default();
        let primary_choice = choices.first().cloned().unwrap_or(serde_json::json!({}));

        let usage_event = ProviderEvent::UsageUpdate {
            timestamp: Utc::now(),
            prompt_tokens: response_body["usage"]["prompt_tokens"].as_u64(),
            completion_tokens: response_body["usage"]["completion_tokens"].as_u64(),
            total_tokens: response_body["usage"]["total_tokens"].as_u64(),
            cached_tokens: response_body["usage"]
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|t| t.as_u64()),
            reasoning_tokens: response_body["usage"]
                .get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|t| t.as_u64()),
            raw_usage: response_body.get("usage").cloned(),
        };

        let mut events = vec![ProviderEvent::Started {
            timestamp: started,
            provider: provider_name,
            model: model_name,
        }];

        for (choice_idx, choice) in choices.iter().enumerate() {
            let choice_index = choice["index"].as_u64().unwrap_or(choice_idx as u64) as u32;
            let message = &choice["message"];

            let content = message["content"].as_str().unwrap_or_default().to_string();
            if !content.is_empty() {
                events.push(ProviderEvent::ContentDelta {
                    timestamp: Utc::now(),
                    index: choice_index,
                    delta: content,
                });
            }

            if choice_index == 0 {
                let reasoning = message
                    .get("reasoning_content")
                    .or_else(|| message.get("reasoning"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if !reasoning.is_empty() {
                    events.push(ProviderEvent::ReasoningDelta {
                        timestamp: Utc::now(),
                        delta: reasoning,
                    });
                }
            }

            if let Some(tool_calls) = message["tool_calls"].as_array() {
                for (tc_idx, tc) in tool_calls.iter().enumerate() {
                    events.push(ProviderEvent::ToolCallDelta {
                        timestamp: Utc::now(),
                        index: choice_index,
                        tool_call_index: tc["index"].as_u64().unwrap_or(tc_idx as u64) as u32,
                        id: tc["id"].as_str().map(|s| s.to_string()),
                        name: tc["function"]["name"].as_str().map(|s| s.to_string()),
                        arguments: tc["function"]["arguments"].as_str().map(|s| s.to_string()),
                    });
                }
            }
        }

        events.push(usage_event);
        events.push(ProviderEvent::Completed {
            timestamp: Utc::now(),
            finish_reason: primary_choice["finish_reason"].as_str().map(|s| s.to_string()),
            provider_response_id: response_body["id"].as_str().map(|s| s.to_string()),
            upstream_provider: None,
            raw_response: Some(response_body.clone()),
        });

        events
    }
}

#[async_trait]
impl Provider for OpenAICompatibleAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let url = self.endpoint("/v1/models");
        let headers = self.build_headers()?;

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Models request failed with status {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        let body: serde_json::Value = response.json().await?;
        let models = body["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m["id"].as_str()?;
                        Some(ModelInfo::new(id, vec![self.name.clone()], None))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn health(&self) -> ProviderHealth {
        // Try to reach the models endpoint as a health check.
        let url = self.endpoint("/v1/models");
        let headers = match self.build_headers() {
            Ok(h) => h,
            // A malformed api_key genuinely means this provider is
            // misconfigured — Unhealthy is the correct signal, not a panic.
            Err(e) => {
                tracing::warn!(provider = %self.name, error = %e, "Health check failed: invalid headers");
                return ProviderHealth::Unhealthy;
            }
        };

        match self.client.get(&url).headers(headers).send().await {
            Ok(resp) if resp.status().is_success() => ProviderHealth::Healthy,
            Ok(resp) if resp.status().is_server_error() => ProviderHealth::Unhealthy,
            Ok(_) => ProviderHealth::Degraded,
            Err(_) => ProviderHealth::Unhealthy,
        }
    }

    async fn execute(
        &self,
        request: &GatewayRequest,
        model: &str,
    ) -> anyhow::Result<ProviderOutput> {
        let started = Utc::now();
        let body = self.build_chat_body(request, model);
        let url = self.endpoint("/v1/chat/completions");
        let headers = self.build_headers()?;

        let provider_name = self.name.clone();
        let model_name = model.to_string();

        // For non-streaming requests, make a simple request/response.
        if !request.stream {
            let response = self
                .client
                .post(&url)
                .headers(headers)
                .json(&body)
                .send()
                .await?;

            let status = response.status().as_u16();
            let response_body: serde_json::Value = response.json().await?;

            if status >= 400 {
                return Ok(ProviderOutput::Complete(ProviderEvent::Error {
                    timestamp: Utc::now(),
                    code: status.to_string(),
                    message: response_body
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error")
                        .to_string(),
                    status_code: Some(status),
                    retryable: status == 429 || status >= 500,
                }));
            }

            let events = Self::events_from_non_streaming_response(
                &response_body,
                started,
                provider_name,
                model_name,
            );

            return Ok(ProviderOutput::Stream(Box::pin(stream::iter(events))));
        }

        // ── Streaming path ───────────────────────────────────────────────
        // For streaming, we return a Stream that yields ProviderEvents
        // as SSE chunks arrive from the provider.

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if status >= 400 {
            let error_body = response.text().await.unwrap_or_default();
            let error_msg = serde_json::from_str::<serde_json::Value>(&error_body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or(error_body);

            return Ok(ProviderOutput::Complete(ProviderEvent::Error {
                timestamp: Utc::now(),
                code: status.to_string(),
                message: error_msg,
                status_code: Some(status),
                retryable: status == 429 || status >= 500,
            }));
        }

        let connected_at = Utc::now();
        let connection_latency = (connected_at - started)
            .num_microseconds()
            .unwrap_or(0) as u64;

        // Parse the SSE byte stream into ProviderEvents.
        let byte_stream = response.bytes_stream();

        let event_stream = async_stream::stream! {
            yield ProviderEvent::Started {
                timestamp: started,
                provider: provider_name.clone(),
                model: model_name.clone(),
            };

            yield ProviderEvent::Connected {
                timestamp: connected_at,
                connection_latency_us: connection_latency,
            };

            use futures::StreamExt;
            let mut byte_stream = std::pin::pin!(byte_stream);
            let mut byte_buffer = Vec::new();
            let mut first_byte_seen = false;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = match chunk_result {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        yield ProviderEvent::Error {
                            timestamp: Utc::now(),
                            code: "stream_error".to_string(),
                            message: e.to_string(),
                            status_code: None,
                            retryable: false,
                        };
                        return;
                    }
                };

                if !first_byte_seen {
                    first_byte_seen = true;
                    yield ProviderEvent::FirstByte { timestamp: Utc::now() };
                }

                byte_buffer.extend_from_slice(&chunk);

                // Process complete SSE lines split strictly on b'\n' to preserve multi-byte UTF-8 boundaries.
                while let Some(pos) = byte_buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes = &byte_buffer[..pos];
                    let trimmed_bytes = if line_bytes.ends_with(b"\r") {
                        &line_bytes[..line_bytes.len() - 1]
                    } else {
                        line_bytes
                    };
                    let line = String::from_utf8_lossy(trimmed_bytes).trim().to_string();
                    byte_buffer.drain(..=pos);

                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }

                    if let Some(raw_data) = line.strip_prefix("data:") {
                        let data = raw_data.trim();
                        if data == "[DONE]" {
                            // Final event — stream complete.
                            continue;
                        }

                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                            let now = Utc::now();

                            // Iterate every entry in `choices`, not just
                            // index 0 — with `n > 1`, a chunk can carry
                            // deltas for more than one choice, each tagged
                            // with its own `index` (docs/REMEDIATION_PLAN.md
                            // Phase 2.3 / Audit A08). Reasoning is only
                            // captured for choice 0: `ReasoningDelta` has no
                            // per-choice index in the event model, and
                            // reasoning-capable models are not commonly
                            // combined with `n > 1` — documented scoping,
                            // not an oversight.
                            if let Some(choices) = parsed["choices"].as_array() {
                                for choice in choices {
                                    let choice_index = choice["index"].as_u64().unwrap_or(0) as u32;
                                    let delta = &choice["delta"];

                                    if let Some(delta_content) = delta["content"].as_str()
                                        && !delta_content.is_empty()
                                    {
                                        yield ProviderEvent::ContentDelta {
                                            timestamp: now,
                                            index: choice_index,
                                            delta: delta_content.to_string(),
                                        };
                                    }

                                    if choice_index == 0
                                        && let Some(reasoning) = delta
                                            .get("reasoning_content")
                                            .or_else(|| delta.get("reasoning"))
                                            .and_then(|v| v.as_str())
                                        && !reasoning.is_empty()
                                    {
                                        yield ProviderEvent::ReasoningDelta {
                                            timestamp: now,
                                            delta: reasoning.to_string(),
                                        };
                                    }

                                    if let Some(tool_calls) = delta["tool_calls"].as_array() {
                                        for tc in tool_calls {
                                            yield ProviderEvent::ToolCallDelta {
                                                timestamp: now,
                                                index: choice_index,
                                                tool_call_index: tc["index"].as_u64().unwrap_or(0) as u32,
                                                id: tc["id"].as_str().map(|s| s.to_string()),
                                                name: tc["function"]["name"].as_str().map(|s| s.to_string()),
                                                arguments: tc["function"]["arguments"].as_str().map(|s| s.to_string()),
                                            };
                                        }
                                    }

                                    if let Some(reason) = choice["finish_reason"].as_str() {
                                        yield ProviderEvent::Completed {
                                            timestamp: now,
                                            finish_reason: Some(reason.to_string()),
                                            provider_response_id: parsed["id"].as_str().map(|s| s.to_string()),
                                            upstream_provider: None,
                                            // A real SSE stream has no single
                                            // response body to preserve here —
                                            // each chunk was already forwarded
                                            // as its own StreamChunk telemetry
                                            // event.
                                            raw_response: None,
                                        };
                                    }
                                }
                            }

                            // Extract usage (if present in stream).
                            if let Some(usage) = parsed.get("usage") {
                                yield ProviderEvent::UsageUpdate {
                                    timestamp: now,
                                    prompt_tokens: usage["prompt_tokens"].as_u64(),
                                    completion_tokens: usage["completion_tokens"].as_u64(),
                                    total_tokens: usage["total_tokens"].as_u64(),
                                    cached_tokens: usage
                                        .get("prompt_tokens_details")
                                        .and_then(|d| d.get("cached_tokens"))
                                        .and_then(|t| t.as_u64()),
                                    reasoning_tokens: usage
                                        .get("completion_tokens_details")
                                        .and_then(|d| d.get("reasoning_tokens"))
                                        .and_then(|t| t.as_u64()),
                                    raw_usage: Some(usage.clone()),
                                };
                            }
                        }
                    }
                }
            }
        };

        Ok(ProviderOutput::Stream(Box::pin(event_stream)))
    }

    async fn embeddings(
        &self,
        model: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let url = self.endpoint("/v1/embeddings");
        let headers = self.build_headers()?;
        let payload = serde_json::json!({
            "model": model,
            "input": input,
        });

        let resp = self.client.post(&url).headers(headers).json(&payload).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Provider '{}' embeddings returned error {}: {}", self.name, status, text);
        }

        let res: serde_json::Value = resp.json().await?;
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConfig;

    fn config_with_api_key(api_key: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: "test".to_string(),
            r#type: crate::types::ProviderType::OpenAiCompatible,
            base_url: url::Url::parse("http://127.0.0.1:1/v1").unwrap(),
            api_key: api_key.map(str::to_string),
            api_key_env: None,
            default_headers: None,
            timeout_secs: 30,
            max_concurrent: None,
            rate_limit_per_sec: None,
            degraded_error_rate_threshold: None,
            circuit_breaker_threshold: None,
            circuit_breaker_cooldown_secs: 30,
            enabled: true,
        }
    }

    #[test]
    fn build_headers_succeeds_with_a_normal_api_key() {
        let adapter =
            OpenAICompatibleAdapter::new(&config_with_api_key(Some("sk-normal-key")), ModelCapabilities::default())
                .unwrap();
        assert!(adapter.build_headers().is_ok());
    }

    #[test]
    fn build_headers_returns_an_error_instead_of_panicking_on_a_malformed_api_key() {
        // A stray newline (e.g. from a copy-pasted key with a trailing
        // linebreak) is not a valid HTTP header value byte. This used to
        // `.unwrap()` and panic on every single request to the provider
        // until the operator fixed their config — now it's a clean error
        // (docs/REMEDIATION_PLAN.md Phase 5 hardening pass).
        let adapter =
            OpenAICompatibleAdapter::new(&config_with_api_key(Some("sk-bad\nkey")), ModelCapabilities::default())
                .unwrap();
        let err = adapter.build_headers().unwrap_err();
        assert!(err.to_string().contains("test"), "{err}");
    }

    #[test]
    fn build_headers_succeeds_with_no_api_key() {
        let adapter = OpenAICompatibleAdapter::new(&config_with_api_key(None), ModelCapabilities::default()).unwrap();
        assert!(adapter.build_headers().is_ok());
    }
}
