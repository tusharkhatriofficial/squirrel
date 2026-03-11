//! SettingsHandler — kernel-side agent that applies settings changes.
//!
//! The WASM settings-module sends "settings.set" intents with payload
//! "key\0value" and the input-module sends "input.secret.response" with
//! the plaintext API key. This agent receives both and writes them to
//! the OsSettings store (which persists to SVFS).

use alloc::string::String;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};

pub struct SettingsHandler;

impl SettingsHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Agent for SettingsHandler {
    fn name(&self) -> &str {
        "settings-handler"
    }

    fn priority(&self) -> CognitivePriority {
        CognitivePriority::Background
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        let intent = match ctx.bus.try_recv() {
            Some(i) => i,
            None => return AgentPoll::Pending,
        };

        let intent_type = intent.semantic_type.as_str();

        match intent_type {
            "settings.set" => {
                // Payload format: "key\0value" (null-separated)
                let payload = &intent.payload;
                if let Some(sep) = payload.iter().position(|&b| b == 0) {
                    let key = core::str::from_utf8(&payload[..sep]).unwrap_or("");
                    let value = core::str::from_utf8(&payload[sep + 1..]).unwrap_or("");
                    if let Some(s) = settings::OS_SETTINGS.get() {
                        if let Err(_) = s.set(key, value) {
                            crate::println!("[Settings] Failed to set {}={}", key, value);
                        }
                    }
                }
            }
            "input.secret.response" => {
                // Payload is the plaintext API key
                let key_text = core::str::from_utf8(&intent.payload).unwrap_or("");
                if !key_text.is_empty() {
                    if let Some(s) = settings::OS_SETTINGS.get() {
                        match s.set_api_key(key_text) {
                            Ok(()) => {}
                            Err(_) => crate::println!("[Settings] Failed to save API key"),
                        }
                    }
                }
            }
            _ => {}
        }

        AgentPoll::Yield
    }
}
