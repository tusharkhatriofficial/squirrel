//! Squirrel AIOS AI Inference Engine
//!
//! The inference engine provides a unified interface for AI text generation.
//! It supports two backend types:
//!
//!   - **Local** (Stage 2): Run GGUF models directly on the CPU via llama.cpp
//!     or candle. Currently stubbed — returns NoLocalModel.
//!
//!   - **Cloud API** (MVP): Send prompts to OpenAI, Anthropic, or Gemini via
//!     the network stack's HTTP client.
//!
//! Architecture:
//!
//!   ┌─────────────────────────────────────────┐
//!   │  InferenceRouter (SART agent)           │  Subscribes to "inference.generate"
//!   ├─────────────────────────────────────────┤
//!   │  get_current_backend()                  │  Reads OS Settings (stub for MVP)
//!   ├──────────┬──────────────────────────────┤
//!   │  Local   │  Cloud API                   │
//!   │  (stub)  │  OpenAI / Anthropic / Gemini │
//!   └──────────┴──────────────────────────────┘
//!
//! The router reads settings FRESH on every request, so backend switching
//! is live — no restart needed.

#![no_std]
extern crate alloc;

pub mod backend;
pub mod backends;
pub mod router;

pub use backend::{InferenceBackend, InferenceError, InferenceRequest, InferenceResponse};
pub use backends::api::ApiProvider;
pub use router::InferenceRouter;

use alloc::string::String;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Settings stubs — replaced by OS Settings in Phase 11
// ---------------------------------------------------------------------------

/// The currently selected backend name.
///
/// Defaults to "local" (which will fail gracefully and tell the user to
/// configure a cloud API). In Phase 11, this will be read from OS Settings
/// instead of this global.
///
/// Valid values: "local", "openai", "anthropic", "gemini", "custom"
static BACKEND_SETTING: Mutex<String> = Mutex::new(String::new());

/// API key for the currently selected cloud provider.
///
/// Empty by default — must be set via OS Settings (Phase 11) or
/// programmatically for testing. The inference engine NEVER logs or
/// exposes this value through the Glass Box.
static API_KEY_SETTING: Mutex<String> = Mutex::new(String::new());

/// Model ID for the currently selected provider.
///
/// Default values per provider:
///   - OpenAI: "gpt-4o"
///   - Anthropic: "claude-sonnet-4-20250514"
///   - Gemini: "gemini-pro"
///   - Custom: user-specified
static MODEL_ID_SETTING: Mutex<String> = Mutex::new(String::new());

/// Base URL for custom API backends (OpenAI-compatible endpoints).
///
/// Only used when backend is "custom". Examples:
///   - "http://localhost:11434/v1/chat/completions" (Ollama)
///   - "http://localhost:8080/v1/chat/completions" (local proxy)
static BASE_URL_SETTING: Mutex<String> = Mutex::new(String::new());

/// Get the currently configured backend name.
///
/// Called by the InferenceRouter on every request to enable live switching.
/// Returns "local" if no backend has been configured yet.
pub fn get_current_backend() -> String {
    let setting = BACKEND_SETTING.lock();
    if setting.is_empty() {
        String::from("local")
    } else {
        setting.clone()
    }
}

/// Get the API settings for a given backend name.
///
/// Returns (api_key, model_id, base_url, provider).
/// In Phase 11, these will come from OS Settings instead of globals.
pub fn get_api_settings(
    backend: &str,
) -> (String, String, String, ApiProvider) {
    let api_key = API_KEY_SETTING.lock().clone();
    let model_id = MODEL_ID_SETTING.lock().clone();
    let base_url = BASE_URL_SETTING.lock().clone();

    let provider = match backend {
        "openai" => ApiProvider::OpenAi,
        "anthropic" => ApiProvider::Anthropic,
        "gemini" => ApiProvider::Gemini,
        _ => ApiProvider::Custom,
    };

    // Use sensible defaults if model_id is empty
    let model_id = if model_id.is_empty() {
        match provider {
            ApiProvider::OpenAi => String::from("gpt-4o"),
            ApiProvider::Anthropic => String::from("claude-sonnet-4-20250514"),
            ApiProvider::Gemini => String::from("gemini-pro"),
            ApiProvider::Custom => String::from("default"),
        }
    } else {
        model_id
    };

    (api_key, model_id, base_url, provider)
}

/// Set the inference backend programmatically.
///
/// This is a temporary API for testing. In Phase 11, the OS Settings
/// module will manage these values and the InferenceRouter will read
/// them directly from settings.
pub fn configure(backend: &str, api_key: &str, model_id: &str, base_url: &str) {
    *BACKEND_SETTING.lock() = String::from(backend);
    *API_KEY_SETTING.lock() = String::from(api_key);
    *MODEL_ID_SETTING.lock() = String::from(model_id);
    *BASE_URL_SETTING.lock() = String::from(base_url);
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

static LOG_FN: spin::Once<fn(&str)> = spin::Once::new();

/// Set the logging function (called by the kernel at init time).
pub fn set_log_fn(f: fn(&str)) {
    LOG_FN.call_once(|| f);
}

/// Internal println! macro that routes to the kernel's framebuffer display.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use alloc::format;
        let msg = format!($($arg)*);
        if let Some(f) = $crate::LOG_FN.get() {
            f(&msg);
        }
    }};
}
