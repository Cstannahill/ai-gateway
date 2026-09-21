use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ─── Gateway Request ─────────────────────────────────────────────────────────
//
// The canonical internal request representation. Inbound requests (OpenAI-compat,
// etc.) are parsed into this before routing and dispatch.

/// The canonical gateway request — provider-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayRequest {
    /// Unique request identifier (UUIDv7 for time-ordering).
    #[serde(default = "generate_request_id")]
    pub request_id: Uuid,

    /// Timestamp when the gateway received this request.
    #[serde(default = "Utc::now")]
    pub received_at: DateTime<Utc>,

    /// The model identifier — can be an alias (e.g. "coding-fast") or an
    /// explicit provider-qualified ID (e.g. "openrouter/anthropic/claude-...").
    pub model: String,

    /// Chat messages (Chat Completions style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,

    /// Input (Responses API style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,

    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Nucleus sampling (top-p).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Maximum tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,

    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// Whether to stream the response.
    #[serde(default)]
    pub stream: bool,

    /// Tool definitions for function calling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,

    /// Tool choice constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,

    /// Structured output / JSON schema constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// Reasoning / thinking configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,

    /// Requested modalities (text, image, audio, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,

    /// Number of completions to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// Frequency penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,

    /// Presence penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,

    /// Seed for deterministic generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,

    /// User identifier (for rate limiting / tracking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    // ─── Escape hatch ────────────────────────────────────────────────────
    /// Provider-specific parameters that don't fit the canonical schema.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "llama_cpp": { "min_p": 0.05, "repeat_penalty": 1.05 },
    ///   "vllm": { "top_k": 40 }
    /// }
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<HashMap<String, serde_json::Value>>,

    // ─── Gateway metadata ────────────────────────────────────────────────
    /// Routing hints (e.g. explicit provider, policy override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingHints>,

    /// User-defined metadata for tagging / tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RequestMetadata>,
}

fn generate_request_id() -> Uuid {
    Uuid::now_v7()
}

impl Default for GatewayRequest {
    fn default() -> Self {
        Self {
            request_id: generate_request_id(),
            received_at: Utc::now(),
            model: String::new(),
            messages: None,
            input: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
            response_format: None,
            reasoning: None,
            modalities: None,
            n: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            user: None,
            provider_options: None,
            routing: None,
            metadata: None,
        }
    }
}

// ─── Messages ────────────────────────────────────────────────────────────────

/// A chat message in the canonical format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Message content — can be a simple string or multimodal parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// A content part within a multimodal message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ─── Tools ───────────────────────────────────────────────────────────────────

/// A tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub r#type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// A tool call in a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// ─── Structured Output ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseFormat {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "json_object")]
    JsonObject,
    #[serde(rename = "json_schema")]
    JsonSchema { json_schema: JsonSchemaConfig },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchemaConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

// ─── Reasoning ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningConfig {
    /// Whether reasoning/thinking is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum reasoning tokens (budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Effort level hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// ─── Routing Hints ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingHints {
    /// Force a specific provider (bypass routing engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Override the routing policy for this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,

    /// Explicit candidate list override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
}

// ─── Request Metadata ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetadata {
    /// Application identifier (e.g. "my-coding-agent").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,

    /// Task identifier (e.g. "code-review", "summarization").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,

    /// User-defined tags for filtering and analysis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Arbitrary key-value labels.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}
