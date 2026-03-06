//! Settings schema — defines the structure of all OS settings.
//!
//! SquirrelSettings is the root document. It contains sub-sections for
//! different parts of the OS. Right now there are two sections:
//!
//!   [inference] — which AI backend to use (local model vs cloud API)
//!   [display]   — visual preferences (Glass Box overlay, boot animation)
//!
//! Settings are stored as TOML text in SVFS. We have a simple hand-written
//! TOML serializer/parser here because the full `toml` crate doesn't work
//! in no_std. The format is simple enough that we don't need a full parser.

use alloc::string::String;

/// The complete settings document — everything Squirrel needs to remember.
///
/// This gets serialized to TOML and stored in SVFS with the tag "os-settings".
/// On boot, we load it from SVFS. If it doesn't exist (first boot), we use
/// Default::default() which gives sensible out-of-the-box values.
#[derive(Debug, Clone)]
pub struct SquirrelSettings {
    /// AI inference configuration — the most important section.
    /// Controls whether we run a local model or call a cloud API.
    pub inference: InferenceSettings,

    /// Display preferences — cosmetic settings that don't affect AI behavior.
    pub display: DisplaySettings,
}

impl Default for SquirrelSettings {
    fn default() -> Self {
        Self {
            inference: InferenceSettings::default(),
            display: DisplaySettings::default(),
        }
    }
}

/// Inference backend settings — controls how AI requests are fulfilled.
///
/// The InferenceRouter reads these on EVERY request (not cached in the router),
/// which means changing `backend` from "local" to "anthropic" takes effect
/// immediately on the next AI prompt — no reboot, no restart.
#[derive(Debug, Clone)]
pub struct InferenceSettings {
    /// Which backend to use. Valid values:
    ///   "local"     — run a GGUF model on the CPU (Phase 10 local backend)
    ///   "openai"    — call OpenAI's API (GPT-4o, etc.)
    ///   "anthropic" — call Anthropic's API (Claude)
    ///   "gemini"    — call Google's Gemini API
    ///   "custom"    — call a user-specified OpenAI-compatible endpoint
    pub backend: String,

    /// Cloud model ID — tells the API which model to use.
    /// Examples: "gpt-4o", "claude-sonnet-4-6", "gemini-2.0-flash"
    /// Ignored when backend is "local".
    pub model_id: String,

    /// Custom API base URL — only used when backend is "custom".
    /// Example: "http://localhost:11434/v1/chat/completions" for Ollama.
    /// Empty string for standard providers (they have hardcoded URLs).
    pub api_base_url: String,

    /// SVFS content hash (hex) of the local GGUF model file.
    /// Set automatically when a model is installed. Empty if no local
    /// model is available.
    pub local_model_hash: String,

    /// SVFS content hash (hex) of the ENCRYPTED API key blob.
    /// This is NOT the API key — it's the hash of the encrypted blob
    /// stored in SVFS. The actual key is encrypted with AES-256-GCM
    /// using a machine-specific key derived from CPUID.
    /// Empty string means no API key has been configured.
    pub api_key_ref: String,
}

impl Default for InferenceSettings {
    fn default() -> Self {
        Self {
            backend: String::from("local"),
            model_id: String::from("claude-sonnet-4-6"),
            api_base_url: String::new(),
            local_model_hash: String::new(),
            api_key_ref: String::new(),
        }
    }
}

/// Display settings — visual preferences.
#[derive(Debug, Clone)]
pub struct DisplaySettings {
    /// Whether the Glass Box overlay is visible during AI execution.
    /// Glass Box shows real-time internal state of all running agents.
    pub glass_box_visible: bool,

    /// Whether to show the loading animation on boot.
    pub boot_animation: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            glass_box_visible: true,
            boot_animation: true,
        }
    }
}

impl SquirrelSettings {
    /// Serialize settings to a TOML string for storage in SVFS.
    ///
    /// We write this by hand because the `toml` crate requires std.
    /// The format is simple: section headers + key = "value" pairs.
    pub fn to_toml(&self) -> String {
        alloc::format!(
            "[inference]\n\
             backend = \"{}\"\n\
             model_id = \"{}\"\n\
             api_base_url = \"{}\"\n\
             local_model_hash = \"{}\"\n\
             api_key_ref = \"{}\"\n\
             \n\
             [display]\n\
             glass_box_visible = {}\n\
             boot_animation = {}\n",
            self.inference.backend,
            self.inference.model_id,
            self.inference.api_base_url,
            self.inference.local_model_hash,
            self.inference.api_key_ref,
            self.display.glass_box_visible,
            self.display.boot_animation,
        )
    }

    /// Parse settings from a TOML string.
    ///
    /// This is a minimal line-by-line parser. It doesn't validate TOML
    /// syntax — it just looks for "key = value" pairs and maps them to
    /// the known setting names. Unknown keys are silently ignored.
    /// String values have their surrounding quotes stripped.
    /// Boolean values are compared against the string "true".
    pub fn from_toml(toml: &str) -> Self {
        let mut settings = Self::default();

        for line in toml.lines() {
            let line = line.trim();

            // Skip section headers and comments
            if line.starts_with('[') || line.starts_with('#') || line.is_empty() {
                continue;
            }

            // Split on first '=' to get key and value
            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim();
                let val = val.trim().trim_matches('"');

                match key {
                    "backend" => settings.inference.backend = String::from(val),
                    "model_id" => settings.inference.model_id = String::from(val),
                    "api_base_url" => settings.inference.api_base_url = String::from(val),
                    "local_model_hash" => settings.inference.local_model_hash = String::from(val),
                    "api_key_ref" => settings.inference.api_key_ref = String::from(val),
                    "glass_box_visible" => settings.display.glass_box_visible = val == "true",
                    "boot_animation" => settings.display.boot_animation = val == "true",
                    _ => {} // Unknown keys are silently ignored
                }
            }
        }

        settings
    }
}
