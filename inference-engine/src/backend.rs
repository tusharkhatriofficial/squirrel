//! Core inference types — the universal interface for all AI backends.
//!
//! Every inference backend (local llama.cpp, OpenAI API, Anthropic API,
//! Gemini API) implements the `InferenceBackend` trait. The InferenceRouter
//! picks the active backend based on OS Settings and dispatches requests.
//!
//! These types are also used as Intent Bus payloads: when the Primary AI
//! Agent needs to think, it sends an InferenceRequest intent and receives
//! an InferenceResponse intent.

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// The single interface all inference backends implement.
///
/// Backends handle the full lifecycle: take a prompt, run inference
/// (locally or via API), and return generated text.
pub trait InferenceBackend: Send {
    /// Run inference on the given request. Returns generated text.
    fn generate(
        &mut self,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError>;

    /// Human-readable name of this backend (for Glass Box display).
    fn name(&self) -> &str;

    /// Whether this backend is currently able to handle requests.
    fn is_available(&self) -> bool;
}

/// A request for AI text generation.
///
/// Sent as the payload of "inference.generate" intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    /// The full prompt (system + user message, pre-assembled)
    pub prompt: String,
    /// Maximum tokens to generate
    pub max_tokens: usize,
    /// Sampling temperature (0.0 = deterministic, 1.0 = creative)
    pub temperature: f32,
    /// Stop generation when any of these sequences appear
    pub stop_sequences: Vec<String>,
}

/// The result of AI text generation.
///
/// Sent as the payload of "inference.generate.response" intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResponse {
    /// The generated text
    pub text: String,
    /// Number of tokens generated
    pub tokens_generated: usize,
    /// Why generation stopped
    pub finish_reason: FinishReason,
    /// Which backend handled this request (for Glass Box display)
    pub backend_used: String,
    /// Wall-clock latency in milliseconds
    pub latency_ms: u64,
}

/// Why the AI stopped generating text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinishReason {
    /// Hit one of the requested stop sequences
    StopSequence,
    /// Reached the max_tokens limit
    MaxTokens,
    /// Model naturally ended (EOS token or API signal)
    EndOfSequence,
}

/// Errors that can occur during inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InferenceError {
    /// No local model file found in SVFS
    NoLocalModel,
    /// Network stack not initialized or unreachable
    NetworkUnavailable,
    /// Cloud API returned an error
    ApiError(String),
    /// Failed to load the local model
    ModelLoadFailed,
    /// Request was malformed
    InvalidRequest,
    /// Inference took too long
    Timeout,
}

impl core::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoLocalModel => write!(
                f,
                "No local model found. Use 'settings' to configure a cloud API."
            ),
            Self::NetworkUnavailable => write!(f, "Network unavailable"),
            Self::ApiError(e) => write!(f, "API error: {}", e),
            Self::ModelLoadFailed => write!(f, "Model load failed"),
            Self::InvalidRequest => write!(f, "Invalid inference request"),
            Self::Timeout => write!(f, "Inference timed out"),
        }
    }
}
