//! Squirrel AIOS OS Settings
//!
//! This crate manages all persistent, runtime-configurable settings for the OS.
//! The most important setting it controls is the AI inference backend — users
//! can switch between local model and cloud APIs without rebooting.
//!
//! Usage from other crates:
//!
//!   // Read which backend is active
//!   let backend = settings::current_backend();  // "local", "openai", etc.
//!
//!   // Get the decrypted API key (only for inference engine)
//!   let key = settings::get_api_key();
//!
//!   // Change a setting
//!   settings::OS_SETTINGS.get().unwrap().set("inference.backend", "anthropic");
//!
//! Architecture:
//!
//!   ┌─────────────────────────────────────────────────────────────┐
//!   │  OS_SETTINGS: Once<OsSettings>  ← global singleton         │
//!   │     │                                                       │
//!   │     ├── schema.rs  — what settings exist (data structures)  │
//!   │     ├── store.rs   — read/write settings (cache + SVFS)     │
//!   │     └── crypto.rs  — encrypt/decrypt API keys (AES-256-GCM) │
//!   └─────────────────────────────────────────────────────────────┘

#![no_std]
extern crate alloc;

pub mod crypto;
pub mod schema;
pub mod store;

pub use schema::{DisplaySettings, InferenceSettings, SquirrelSettings};
pub use store::{OsSettings, SettingsError};

use spin::Once;

// ---------------------------------------------------------------------------
// Logging — same pattern as inference-engine and network crates
// ---------------------------------------------------------------------------

static LOG_FN: Once<fn(&str)> = Once::new();

/// Set the logging function. Called by the kernel at boot time to bridge
/// settings log messages to the framebuffer display.
pub fn set_log_fn(f: fn(&str)) {
    LOG_FN.call_once(|| f);
}

/// Internal println! macro that routes to the kernel's framebuffer.
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

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

/// The global OS Settings instance.
///
/// Initialized once during boot by calling init(). After that, any crate
/// can read settings through OS_SETTINGS.get().
///
/// We use spin::Once (not Mutex) because settings are initialized exactly
/// once and then read many times. Once guarantees initialization happens
/// exactly once, even if multiple threads try to call init() simultaneously.
pub static OS_SETTINGS: Once<OsSettings> = Once::new();

/// Initialize the settings system.
///
/// Must be called after SVFS is initialized (settings are stored in SVFS)
/// and before the inference engine starts (it reads settings).
///
/// On first boot: Writes default settings to SVFS.
/// On subsequent boots: Loads saved settings from SVFS.
pub fn init() {
    OS_SETTINGS.call_once(|| OsSettings::load());
    crate::println!("[OK] OS Settings loaded");
}

/// Get the name of the currently active inference backend.
///
/// This is a convenience function used by the inference engine.
/// Returns "local" if settings haven't been initialized yet.
///
/// Possible return values: "local", "openai", "anthropic", "gemini", "custom"
pub fn current_backend() -> alloc::string::String {
    OS_SETTINGS
        .get()
        .map(|s| s.get_inference_settings().backend)
        .unwrap_or_else(|| alloc::string::String::from("local"))
}

/// Get the decrypted API key.
///
/// This is ONLY for the inference engine to use when making API calls.
/// The returned key must NEVER be:
///   - Logged to the framebuffer
///   - Sent through the Intent Bus
///   - Displayed in the Glass Box
///   - Stored anywhere except briefly in a local variable
pub fn get_api_key() -> Result<alloc::string::String, SettingsError> {
    OS_SETTINGS
        .get()
        .ok_or(SettingsError::NotConfigured)?
        .get_api_key()
}
