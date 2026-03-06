//! InferenceRouter — the SART agent that dispatches AI inference requests.
//!
//! This is the brain of the inference engine. It sits as a SART agent,
//! listening for "inference.generate" intents on the Intent Bus. When one
//! arrives, the router:
//!
//! 1. Reads the current backend setting (local, openai, anthropic, gemini)
//! 2. Creates the appropriate backend
//! 3. Calls generate() on it
//! 4. Sends the result back as an "inference.generate.response" intent
//! 5. Updates the Glass Box with timing and backend info
//!
//! The router reads settings FRESH on every request. This means if a user
//! changes their API key or switches providers in OS Settings, the very
//! next inference request uses the new configuration — no restart needed.
//!
//! Priority: Reasoning (highest). When the AI is thinking, inference
//! should be processed before anything else.

use alloc::format;
use intent_bus::Intent;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};

use crate::backend::{InferenceError, InferenceRequest, InferenceResponse};
use crate::backends::api::ApiInferenceBackend;
use crate::backends::local::LocalInferenceBackend;

/// The Inference Router — dispatches inference requests to the right backend.
pub struct InferenceRouter {
    /// The local inference backend (None if no model is loaded).
    /// Kept across requests so the model stays in memory.
    local_backend: Option<LocalInferenceBackend>,
}

impl InferenceRouter {
    /// Create a new InferenceRouter.
    ///
    /// Attempts to load a local model on startup. If no model is found
    /// (which is always the case in the MVP stub), the router operates
    /// in API-only mode and will use cloud backends.
    pub fn new() -> Self {
        let local = match LocalInferenceBackend::try_load() {
            Ok(backend) => {
                crate::println!("[Inference] Local backend ready");
                Some(backend)
            }
            Err(InferenceError::NoLocalModel) => {
                crate::println!("[Inference] No local model — API-only mode");
                None
            }
            Err(e) => {
                crate::println!("[Inference] Local backend failed: {}", e);
                None
            }
        };

        Self {
            local_backend: local,
        }
    }

    /// Handle a single inference request by routing to the correct backend.
    ///
    /// The backend is selected by reading the current setting from
    /// `crate::get_current_backend()`. This function is called fresh
    /// every time, so settings changes take effect immediately.
    fn handle_generate(
        &mut self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        let backend_name = crate::get_current_backend();

        match backend_name.as_str() {
            "local" => {
                // Try the local backend (always fails in MVP stub)
                if let Some(ref mut backend) = self.local_backend {
                    use crate::backend::InferenceBackend;
                    backend.generate(&request)
                } else {
                    Err(InferenceError::NoLocalModel)
                }
            }
            "openai" | "anthropic" | "gemini" | "custom" => {
                // Create a fresh API backend with current settings
                let (api_key, model_id, base_url, provider) =
                    crate::get_api_settings(&backend_name);

                let mut api_backend =
                    ApiInferenceBackend::new(provider, api_key, model_id, base_url);

                use crate::backend::InferenceBackend;
                if !api_backend.is_available() {
                    return Err(InferenceError::NetworkUnavailable);
                }
                api_backend.generate(&request)
            }
            unknown => Err(InferenceError::ApiError(format!(
                "Unknown backend: '{}'",
                unknown
            ))),
        }
    }
}

impl Agent for InferenceRouter {
    fn name(&self) -> &str {
        "inference-engine"
    }

    fn priority(&self) -> CognitivePriority {
        // Reasoning priority — inference is the highest-priority work.
        // When the AI is thinking, it should get CPU time before network
        // polling, display rendering, or background tasks.
        CognitivePriority::Reasoning
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // Check for pending inference requests
        let intent = match ctx.bus.try_recv() {
            Some(intent) if intent.semantic_type.matches("inference.generate") => intent,
            Some(_) => return AgentPoll::Yield,
            None => return AgentPoll::Pending,
        };

        // Decode the request payload
        let request = match intent.decode::<InferenceRequest>() {
            Ok(req) => req,
            Err(_) => {
                crate::println!("[Inference] Failed to decode request payload");
                return AgentPoll::Yield;
            }
        };

        // Update Glass Box — show that inference is in progress
        let status_update = Intent::request(
            "glass-box.update",
            "inference-engine",
            &glass_box::GlassBoxUpdate {
                module: "inference-engine".into(),
                key: "status".into(),
                value: "generating...".into(),
            },
        );
        ctx.bus.send(status_update);

        // Route the request to the appropriate backend
        let result = self.handle_generate(request);

        match result {
            Ok(response) => {
                // Update Glass Box with result info (NEVER include API keys)
                let backend_update = Intent::request(
                    "glass-box.update",
                    "inference-engine",
                    &glass_box::GlassBoxUpdate {
                        module: "inference-engine".into(),
                        key: "last_backend".into(),
                        value: response.backend_used.clone(),
                    },
                );
                ctx.bus.send(backend_update);

                let latency_update = Intent::request(
                    "glass-box.update",
                    "inference-engine",
                    &glass_box::GlassBoxUpdate {
                        module: "inference-engine".into(),
                        key: "last_latency_ms".into(),
                        value: format!("{}", response.latency_ms),
                    },
                );
                ctx.bus.send(latency_update);

                let status_done = Intent::request(
                    "glass-box.update",
                    "inference-engine",
                    &glass_box::GlassBoxUpdate {
                        module: "inference-engine".into(),
                        key: "status".into(),
                        value: "idle".into(),
                    },
                );
                ctx.bus.send(status_done);

                // Send the response back to the requester
                let reply = Intent::response(&intent, "inference-engine", &response);
                ctx.bus.send(reply);
            }
            Err(e) => {
                crate::println!("[Inference] Error: {}", e);

                // Update Glass Box with error status
                let error_update = Intent::request(
                    "glass-box.update",
                    "inference-engine",
                    &glass_box::GlassBoxUpdate {
                        module: "inference-engine".into(),
                        key: "status".into(),
                        value: format!("error: {}", e),
                    },
                );
                ctx.bus.send(error_update);

                // Send the error back as a response
                let error_text = format!("{}", e);
                let reply = Intent::response(&intent, "inference-engine", &error_text);
                ctx.bus.send(reply);
            }
        }

        AgentPoll::Yield
    }
}
