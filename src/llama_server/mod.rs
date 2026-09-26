// ─── llama-server manager ────────────────────────────────────────────────────
//
// Saves models (gguf + optional mmproj) under user-chosen aliases and starts,
// stops, and reports on a local `llama-server` process. Off by default: it is
// the only part of the gateway that executes a local binary. See
// `docs/frontend/LLAMA_SERVER_UI.md` for the full API/UI contract.

pub mod args;
pub mod logs;
pub mod manager;
pub mod probe;
pub mod store;

pub use manager::{LlamaServerManager, State, StatusSnapshot};

/// Errors surfaced by the manager. Each maps to a stable `code` string and an
/// HTTP status (see `server/routes/llama_server.rs`).
#[derive(Debug)]
pub enum LlamaError {
    /// The feature is not enabled in configuration.
    Disabled,
    NotFound(String),
    /// The request was well-formed but a value is unacceptable (400).
    Invalid { code: &'static str, message: String },
    /// The request conflicts with current state (409).
    Conflict { code: &'static str, message: String },
    /// An operation was attempted and failed (500), e.g. the process would not spawn.
    Failed { code: &'static str, message: String },
    Internal(String),
}

impl LlamaError {
    pub fn invalid(code: &'static str, message: String) -> Self {
        Self::Invalid { code, message }
    }

    pub fn conflict(code: &'static str, message: String) -> Self {
        Self::Conflict { code, message }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "feature_disabled",
            Self::NotFound(_) => "not_found",
            Self::Invalid { code, .. } | Self::Conflict { code, .. } | Self::Failed { code, .. } => code,
            Self::Internal(_) => "internal_error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Disabled => "llama_server management is not enabled in the gateway configuration".to_string(),
            Self::NotFound(m) | Self::Internal(m) => m.clone(),
            Self::Invalid { message, .. } | Self::Conflict { message, .. } | Self::Failed { message, .. } => message.clone(),
        }
    }
}

impl std::fmt::Display for LlamaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for LlamaError {}
