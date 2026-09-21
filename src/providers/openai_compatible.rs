use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
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
    pub fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        let path_clean = path.strip_prefix('/').unwrap_or(path);
        if base.ends_with("/v1") && path_clean.starts_with("v1/") {
            format!("{}/{}", base, &path_clean[3..])
        } else {
            format!("{}/{}", base, path_clean)
        }
    }

    /// Build common headers (Authorization, Content-Type, etc.).
    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        if let Some(ref key) = self.api_key {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", key).parse().unwrap(),
            );
        }

        for (k, v) in &self.default_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        }

        headers
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
        if let Some(ref stop) = request.stop {
            body["stop"] = serde_json::to_value(stop).unwrap_or_default();
        }
        if let Some(ref tools) = request.tools {
            body["tools"] = serde_json::to_value(tools).unwrap_or_default();
        }
        if let Some(ref tool_choice) = request.tool_choice {
            body["tool_choice"] = tool_choice.clone();
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
        let headers = self.build_headers();

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
        let headers = self.build_headers();

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
        let headers = self.build_headers();

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

            // Build content, reasoning, tool_calls and usage events from the response.
            let choice = &response_body["choices"][0];
            let message = &choice["message"];
            let content = message["content"].as_str().unwrap_or_default().to_string();

            let reasoning = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

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

            let mut events = vec![
                ProviderEvent::Started {
                    timestamp: started,
                    provider: provider_name,
                    model: model_name,
                },
            ];

            if !content.is_empty() {
                events.push(ProviderEvent::ContentDelta {
                    timestamp: Utc::now(),
                    index: 0,
                    delta: content,
                });
            }

            if !reasoning.is_empty() {
                events.push(ProviderEvent::ReasoningDelta {
                    timestamp: Utc::now(),
                    delta: reasoning,
                });
            }

            if let Some(tool_calls) = message["tool_calls"].as_array() {
                for (tc_idx, tc) in tool_calls.iter().enumerate() {
                    events.push(ProviderEvent::ToolCallDelta {
                        timestamp: Utc::now(),
                        index: 0,
                        tool_call_index: tc["index"].as_u64().unwrap_or(tc_idx as u64) as u32,
                        id: tc["id"].as_str().map(|s| s.to_string()),
                        name: tc["function"]["name"].as_str().map(|s| s.to_string()),
                        arguments: tc["function"]["arguments"].as_str().map(|s| s.to_string()),
                    });
                }
            }

            events.push(usage_event);
            events.push(ProviderEvent::Completed {
                timestamp: Utc::now(),
                finish_reason: choice["finish_reason"].as_str().map(|s| s.to_string()),
                provider_response_id: response_body["id"].as_str().map(|s| s.to_string()),
                upstream_provider: None,
            });

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

                            // Extract content delta.
                            if let Some(delta_content) = parsed["choices"][0]["delta"]["content"].as_str()
                                && !delta_content.is_empty()
                            {
                                yield ProviderEvent::ContentDelta {
                                    timestamp: now,
                                    index: parsed["choices"][0]["index"].as_u64().unwrap_or(0) as u32,
                                    delta: delta_content.to_string(),
                                };
                            }

                            // Extract reasoning delta (if present).
                            if let Some(reasoning) = parsed["choices"][0]["delta"]
                                .get("reasoning_content")
                                .or_else(|| parsed["choices"][0]["delta"].get("reasoning"))
                                .and_then(|v| v.as_str())
                                && !reasoning.is_empty()
                            {
                                yield ProviderEvent::ReasoningDelta {
                                    timestamp: now,
                                    delta: reasoning.to_string(),
                                };
                            }

                            // Extract tool call deltas.
                            if let Some(tool_calls) = parsed["choices"][0]["delta"]["tool_calls"].as_array() {
                                for tc in tool_calls {
                                    yield ProviderEvent::ToolCallDelta {
                                        timestamp: now,
                                        index: parsed["choices"][0]["index"].as_u64().unwrap_or(0) as u32,
                                        tool_call_index: tc["index"].as_u64().unwrap_or(0) as u32,
                                        id: tc["id"].as_str().map(|s| s.to_string()),
                                        name: tc["function"]["name"].as_str().map(|s| s.to_string()),
                                        arguments: tc["function"]["arguments"].as_str().map(|s| s.to_string()),
                                    };
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

                            // Extract finish reason.
                            if let Some(reason) = parsed["choices"][0]["finish_reason"].as_str() {
                                yield ProviderEvent::Completed {
                                    timestamp: now,
                                    finish_reason: Some(reason.to_string()),
                                    provider_response_id: parsed["id"].as_str().map(|s| s.to_string()),
                                    upstream_provider: None,
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
        let headers = self.build_headers();
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
