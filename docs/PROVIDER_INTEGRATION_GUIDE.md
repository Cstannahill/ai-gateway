# Provider Integration & Escape Hatch Guide

This document explains how each supported LLM provider is integrated into the **AI Gateway**, how vendor-specific features are unlocked via `provider_options`, and how to implement new provider adapters.

---

## 1. Provider Adapter Architecture

All providers implement the asynchronous [`Provider`](file:///s:/Code/RustProjects/AI-Gateway/src/providers/mod.rs#L52-L68) trait:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &ModelCapabilities;
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>>;
    async fn health(&self) -> ProviderHealth;
    async fn execute(&self, request: &GatewayRequest, model: &str) -> anyhow::Result<ProviderOutput>;
    async fn embeddings(&self, model: &str, input: &serde_json::Value) -> anyhow::Result<serde_json::Value>;
}
```

Most providers extend [`OpenAICompatibleAdapter`](file:///s:/Code/RustProjects/AI-Gateway/src/providers/openai_compatible.rs#L25) to inherit standard HTTP connection pooling, SSE streaming parser, and header handling, overriding request enrichment as needed.

---

## 2. Supported Providers & Parameter Escape Hatches

### A. llama.cpp / llama-server

[llama-server](https://github.com/ggerganov/llama.cpp/tree/master/examples/server) provides extremely fast, memory-efficient local GGUF inference on Apple Silicon, NVIDIA GPUs, or CPU.

#### Gateway Configuration
```yaml
providers:
  llama:
    name: "llama"
    type: "llama_cpp"
    base_url: "http://localhost:8080"
    timeout_secs: 300
```

#### Supported Escape Hatch Parameters (`provider_options`)
Pass options under `"llama"`, `"llama_cpp"`, or `"llama-server"`:

| Parameter | Type | Description |
|:---|:---|:---|
| `min_p` | `float` | Minimum probability threshold relative to the most likely token (e.g. `0.05`) |
| `repeat_penalty` | `float` | Penalize repeated tokens (e.g. `1.1`) |
| `top_k` | `integer` | Sample from the top K tokens (e.g. `40`) |
| `typical_p` | `float` | Locally typical sampling threshold |
| `mirostat` | `integer` | Mirostat sampling mode (`0` = disabled, `1` = Mirostat, `2` = Mirostat 2.0) |

#### Example Request
```json
{
  "model": "coding-fast",
  "messages": [{"role": "user", "content": "Write a binary search in Rust"}],
  "provider_options": {
    "llama_cpp": {
      "min_p": 0.05,
      "repeat_penalty": 1.1,
      "top_k": 40
    }
  }
}
```

---

### B. OpenRouter

[OpenRouter](https://openrouter.ai/) provides a unified API for hundreds of hosted proprietary and open-weights models with intelligent multi-host routing.

#### Gateway Configuration
```yaml
providers:
  openrouter:
    name: "openrouter"
    type: "open_router"
    base_url: "https://openrouter.ai/api"
    api_key_env: "OPENROUTER_API_KEY"
    timeout_secs: 120
    # App-attribution headers are sent automatically; see below.
```

#### App attribution
OpenRouter credits usage to an app only when requests carry an `HTTP-Referer`, and shows it under the name in `X-OpenRouter-Title` ([docs](https://openrouter.ai/docs/app-attribution)). The gateway adds both to **every** request it sends to OpenRouter (chat, streaming, `/models`, health checks) with no configuration:

| Header | Default | Notes |
|:---|:---|:---|
| `HTTP-Referer` | `https://ai-gateway.local` | The app's unique identifier at OpenRouter. Changing it starts a new app in their analytics. |
| `X-OpenRouter-Title` | `AI Gateway` | Display name. The legacy `X-Title` is honoured if you set it instead. |
| `X-OpenRouter-Categories` | not sent | Optional, e.g. `cli-agent,personal-agent` (max 2 recognised categories). |
| `X-OpenRouter-App-Visibility` | not sent | Set `hidden` to keep a *new* app off the public rankings (attribution and your own analytics still work; ignored for an app that already exists). |

Override or add any of them per provider with `default_headers`. Names are case-insensitive, and your value replaces the default rather than duplicating it:

```yaml
providers:
  openrouter:
    default_headers:
      "HTTP-Referer": "https://github.com/you/ai-gateway"
      "X-OpenRouter-Title": "My AI Gateway"
      "X-OpenRouter-Categories": "personal-agent"
      "X-OpenRouter-App-Visibility": "hidden"
```

The headers are also applied to any provider, whatever its `type`, whose `base_url` host is `openrouter.ai`, so a generic OpenAI-compatible entry pointed at OpenRouter is attributed too. An invalid header name or value in `default_headers` is a startup error rather than being silently dropped. `tests/openrouter_attribution_tests.rs` verifies the headers on the wire.

#### Supported Escape Hatch Parameters (`provider_options`)
Pass options under `"openrouter"` or `"open_router"`:

| Parameter | Type | Description |
|:---|:---|:---|
| `models` | `array` | Fallback list of models evaluated upstream by OpenRouter |
| `provider` | `object` | Provider routing preferences (e.g. `{"order": ["DeepInfra", "Together"], "allow_fallbacks": false}`) |
| `transforms` | `array` | Upstream prompt transforms |

#### Example Request
```json
{
  "model": "coding-strong",
  "messages": [{"role": "user", "content": "Analyze this architecture"}],
  "provider_options": {
    "openrouter": {
      "provider": {
        "order": ["Anthropic", "Bedrock"],
        "allow_fallbacks": true
      }
    }
  }
}
```

---

### C. vLLM

[vLLM](https://docs.vllm.ai/) is a high-throughput, memory-efficient serving engine for LLMs with PagedAttention.

#### Gateway Configuration
```yaml
providers:
  vllm:
    name: "vllm"
    type: "vllm"
    base_url: "http://localhost:8000"
    timeout_secs: 300
```

#### Supported Escape Hatch Parameters (`provider_options`)
Pass options under `"vllm"`:

| Parameter | Type | Description |
|:---|:---|:---|
| `guided_json` | `object` | JSON Schema string or object for guaranteed schema compliance |
| `guided_regex` | `string` | Regex pattern for constrained decoding |
| `guided_choice` | `array` | Restrict output to a discrete list of string choices |
| `top_k` | `integer` | Top-K sampling |
| `min_p` | `float` | Minimum probability sampling |
| `repetition_penalty` | `float` | Repetition penalty factor |

#### Example Request
```json
{
  "model": "vllm/meta-llama/Llama-3-8B-Instruct",
  "messages": [{"role": "user", "content": "Extract author and date"}],
  "provider_options": {
    "vllm": {
      "guided_json": {
        "type": "object",
        "properties": {
          "author": { "type": "string" },
          "date": { "type": "string" }
        },
        "required": ["author", "date"]
      }
    }
  }
}
```

---

### D. Google Gemini AI

Direct access to Google Gemini models (Gemini 2.5 Flash, Gemini 2.5 Pro) via the Gemini OpenAI-compatible endpoint.

#### Gateway Configuration
```yaml
providers:
  gemini:
    name: "gemini"
    type: "gemini"
    base_url: "https://generativelanguage.googleapis.com/v1beta/openai"
    api_key_env: "GEMINI_API_KEY"
    timeout_secs: 120
```

#### Supported Escape Hatch Parameters (`provider_options`)
Pass options under `"gemini"`:

| Parameter | Type | Description |
|:---|:---|:---|
| `thinkingConfig` | `object` | Thinking budget allocation (e.g. `{"thinkingBudget": 2048}`) |
| `safetySettings` | `array` | Safety threshold overrides |

#### Example Request
```json
{
  "model": "gemini/gemini-2.5-pro",
  "messages": [{"role": "user", "content": "Solve this mathematical proof"}],
  "provider_options": {
    "gemini": {
      "thinkingConfig": {
        "thinkingBudget": 4096
      }
    }
  }
}
```

---

### E. Ollama & Ollama Cloud

Local or remote instances of [Ollama](https://ollama.com/).

#### Gateway Configuration
```yaml
providers:
  ollama:
    name: "ollama"
    type: "ollama"
    base_url: "http://localhost:11434"
    timeout_secs: 300

  ollama_cloud:
    name: "ollama_cloud"
    type: "ollama_cloud"
    base_url: "https://api.ollama.com"
    api_key_env: "OLLAMA_API_KEY"
    timeout_secs: 300
```

---

## 3. How to Add a New Provider Adapter

Adding a new provider takes 3 steps:

### Step 1: Create the Adapter Module
In `src/providers/my_provider.rs`:
```rust
use async_trait::async_trait;
use super::openai_compatible::OpenAICompatibleAdapter;
use super::{Provider, ProviderOutput};
use crate::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderConfig, ProviderHealth};

pub struct MyProviderAdapter {
    inner: OpenAICompatibleAdapter,
}

impl MyProviderAdapter {
    pub fn new(config: &ProviderConfig) -> anyhow::Result<Self> {
        let capabilities = ModelCapabilities {
            tools: true,
            streaming: true,
            structured_output: true,
            vision: false,
            reasoning: false,
            embeddings: false,
            context_window: Some(32768),
            extra: Default::default(),
        };

        Ok(Self {
            inner: OpenAICompatibleAdapter::new(config, capabilities)?,
        })
    }
}

#[async_trait]
impl Provider for MyProviderAdapter {
    fn name(&self) -> &str { self.inner.name() }
    fn capabilities(&self) -> &ModelCapabilities { self.inner.capabilities() }
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> { self.inner.models().await }
    async fn health(&self) -> ProviderHealth { self.inner.health().await }
    async fn execute(&self, req: &GatewayRequest, model: &str) -> anyhow::Result<ProviderOutput> {
        self.inner.execute(req, model).await
    }
    async fn embeddings(&self, model: &str, input: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.inner.embeddings(model, input).await
    }
}
```

### Step 2: Register Provider Type
1. Add variant to [`ProviderType`](file:///s:/Code/RustProjects/AI-Gateway/src/types/provider.rs#L8-L32).
2. Export module in [`src/providers/mod.rs`](file:///s:/Code/RustProjects/AI-Gateway/src/providers/mod.rs#L7-L15).

### Step 3: Instantiate in `src/main.rs`
Add the variant to the initialization match in [`src/main.rs`](file:///s:/Code/RustProjects/AI-Gateway/src/main.rs#L60-L100):
```rust
types::ProviderType::MyProvider => {
    Arc::new(providers::my_provider::MyProviderAdapter::new(&pconfig)?)
}
```

## Live Smoke Tests (opt-in)

Every other test in this repository uses mock providers, so none can catch a real vendor quirk (auth header shape, URL layout, SSE framing, usage reporting). `tests/live_provider_smoke_tests.rs` sends one tiny non-streaming and one streaming chat request (`max_tokens: 64`, a "reply with pong" prompt) per provider, built through `providers::build_adapter` — the same construction path `main.rs` uses.

Each test is a no-op that prints a `SKIP` line unless its trigger environment variable is set, so a plain `cargo test` stays green, offline, and free. Run the ones you configure with output visible:

```bash
LIVE_LLAMA_BASE_URL=http://localhost:8080 OPENROUTER_API_KEY=sk-or-... \
  cargo test --test live_provider_smoke_tests -- --nocapture --test-threads=1
```

| Provider | Trigger | Model (env → default) |
|---|---|---|
| llama.cpp | `LIVE_LLAMA_BASE_URL` | `LIVE_LLAMA_MODEL` → `local` |
| Ollama | `LIVE_OLLAMA_BASE_URL` | `LIVE_OLLAMA_MODEL` (required) |
| vLLM | `LIVE_VLLM_BASE_URL` | `LIVE_VLLM_MODEL` (required) |
| OpenRouter | `OPENROUTER_API_KEY` | `LIVE_OPENROUTER_MODEL` → `openai/gpt-4o-mini` |
| Gemini | `GEMINI_API_KEY` | `LIVE_GEMINI_MODEL` → `gemini-2.0-flash` |
| Hugging Face | `HF_API_KEY` | `LIVE_HF_MODEL` (required) |
| Ollama Cloud | `OLLAMA_API_KEY` | `LIVE_OLLAMA_CLOUD_MODEL` (required); `LIVE_OLLAMA_CLOUD_BASE_URL` → `https://ollama.com` |
| Generic OpenAI-compatible (also how xai/venice/featherless are served) | `LIVE_GENERIC_BASE_URL` | `LIVE_GENERIC_MODEL` (required); `LIVE_GENERIC_API_KEY` optional |

A provider passes when both requests return no `Error` event, reach a `Completed` event, and produce content or reasoning text; the streaming request must also deliver incremental deltas. Health-check status is printed but not asserted (some endpoints, e.g. Hugging Face's router, don't serve `/models`). Cloud targets cost a fraction of a cent per run. Defaults for base URLs and models are unverified starting points — override them via the env vars if a vendor changes its URL or retires a model.
