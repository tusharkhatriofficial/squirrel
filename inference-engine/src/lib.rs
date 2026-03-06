//! Squirrel AIOS AI Inference Engine
//!
//! The inference engine provides a unified interface for AI text generation.
//! It supports two backend types:
//!
//!   - **Local**: Run GGUF models directly on the CPU using a pure-Rust
//!     transformer implementation. Parses GGUF format, dequantizes weights,
//!     runs the full LLaMA-style forward pass with KV-cache.
//!
//!   - **Cloud API**: Send prompts to OpenAI, Anthropic, or Gemini over
//!     HTTPS (TLS 1.3) via the network stack's HttpClient.
//!
//! Architecture:
//!
//!   ┌─────────────────────────────────────────┐
//!   │  InferenceRouter (SART agent)           │  Subscribes to "inference.generate"
//!   ├─────────────────────────────────────────┤
//!   │  get_current_backend()                  │  Reads OS Settings (live)
//!   ├──────────┬──────────────────────────────┤
//!   │  Local   │  Cloud API                   │
//!   │  GGUF    │  OpenAI / Anthropic / Gemini │
//!   └──────────┴──────────────────────────────┘
//!
//! The router reads settings FRESH on every request, so backend switching
//! is live — no restart needed.

#![no_std]
extern crate alloc;

pub mod backend;
pub mod backends;
pub mod gguf;
pub mod router;
pub mod tensor;
pub mod tokenizer;
pub mod transformer;

pub use backend::{InferenceBackend, InferenceError, InferenceRequest, InferenceResponse};
pub use backends::api::ApiProvider;
pub use router::InferenceRouter;

use alloc::string::String;

// ---------------------------------------------------------------------------
// Settings integration — reads from OS Settings (Phase 11)
// ---------------------------------------------------------------------------
// In Phase 10, these were stub globals (Mutex<String>). Now they read from
// the real OS Settings system, which is backed by SVFS for persistence
// and uses AES-256-GCM to protect API keys.

/// Get the currently configured backend name.
///
/// Called by the InferenceRouter on every request to enable live switching.
/// Reads from OS Settings each time — no caching here. When a user changes
/// the backend in settings, the very next inference request uses the new one.
///
/// Returns "local" if settings haven't been initialized yet.
pub fn get_current_backend() -> String {
    settings::current_backend()
}

/// Get the API settings for a given backend name.
///
/// Returns (api_key, model_id, base_url, provider).
/// All values come from OS Settings. The API key is decrypted from SVFS
/// on each call — it's never cached in plaintext in the inference engine.
pub fn get_api_settings(
    backend: &str,
) -> (String, String, String, ApiProvider) {
    let (api_key, model_id, base_url) = if let Some(s) = settings::OS_SETTINGS.get() {
        let inf = s.get_inference_settings();
        let key = s.get_api_key().unwrap_or_default();
        (key, inf.model_id, inf.api_base_url)
    } else {
        (String::new(), String::from("gpt-4o"), String::new())
    };

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
/// Writes to OS Settings, which persists to SVFS immediately. The change
/// takes effect on the next inference request (the router reads settings
/// fresh every time).
pub fn configure(backend: &str, api_key: &str, model_id: &str, base_url: &str) {
    if let Some(s) = settings::OS_SETTINGS.get() {
        s.set("inference.backend", backend).ok();
        s.set("inference.model_id", model_id).ok();
        s.set("inference.api_base_url", base_url).ok();
        if !api_key.is_empty() {
            s.set_api_key(api_key).ok();
        }
    }
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
