//! Primary AI Agent — the top-level intelligence of Squirrel AIOS.
//!
//! This is the brain of the operating system. It receives natural language
//! input from the user, decides what to do (pattern match or AI inference),
//! and routes work to the appropriate capability modules.
//!
//! State machine:
//!   WaitingForInput → user types a line
//!     ├─ pattern match → execute immediately → WaitingForInput
//!     └─ no match → send to inference engine → WaitingForInference
//!   WaitingForInference → AI response arrives → display it → WaitingForInput
//!   WaitingForModule → module signals completion → WaitingForInput
//!
//! Intent flow:
//!   Receives: "input.line", "inference.generate.response", "settings.closed",
//!             "display.clear.done", "system.status"
//!   Sends:    "display.print" (raw text), "inference.generate" (postcard),
//!             "settings.open", "display.clear", "glass-box.update"

#![no_std]
extern crate alloc;

pub mod planner;

use alloc::format;
use alloc::string::String;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use intent_bus::{Intent, IntentPriority, SemanticType};
use inference_engine::InferenceResponse;

use planner::{try_pattern_match, build_inference_request};

/// State machine for the Primary AI Agent.
#[derive(Debug, PartialEq)]
enum AgentState {
    /// Idle — waiting for user to type something.
    WaitingForInput,
    /// Sent an inference request — waiting for the AI to respond.
    WaitingForInference,
    /// Opened a module (settings, etc.) — waiting for it to close.
    WaitingForModule,
}

/// The Primary AI Agent — Squirrel's brain.
pub struct PrimaryAiAgent {
    state: AgentState,
    greeted: bool,
}

impl PrimaryAiAgent {
    pub fn new() -> Self {
        Self {
            state: AgentState::WaitingForInput,
            greeted: false,
        }
    }

    /// Send raw text to the display module.
    ///
    /// This constructs an intent with raw UTF-8 bytes as the payload
    /// (NOT postcard-encoded), because the display module is a WASM
    /// module that reads raw bytes.
    fn print(&self, ctx: &AgentContext, text: &[u8]) {
        let intent = Intent {
            id: 0,
            reply_to: None,
            semantic_type: SemanticType::new("display.print"),
            sender: String::from("primary-agent"),
            payload: text.to_vec(),
            priority: IntentPriority::Normal,
            timestamp_ms: 0,
        };
        ctx.bus.send(intent);
    }

    /// Send a raw-payload intent (for WASM module communication).
    fn send_raw(&self, ctx: &AgentContext, intent_type: &str, payload: &[u8]) {
        let intent = Intent {
            id: 0,
            reply_to: None,
            semantic_type: SemanticType::new(intent_type),
            sender: String::from("primary-agent"),
            payload: payload.to_vec(),
            priority: IntentPriority::Normal,
            timestamp_ms: 0,
        };
        ctx.bus.send(intent);
    }

    /// Update the Glass Box state display.
    fn glass_box(&self, ctx: &AgentContext, key: &str, value: &str) {
        let update = Intent::request(
            "glass-box.update",
            "primary-agent",
            &glass_box::GlassBoxUpdate {
                module: String::from("primary-agent"),
                key: String::from(key),
                value: String::from(value),
            },
        );
        ctx.bus.send(update);
    }
}

impl Agent for PrimaryAiAgent {
    fn name(&self) -> &str {
        "primary-agent"
    }

    fn priority(&self) -> CognitivePriority {
        // Highest priority — the AI's reasoning loop should run first.
        CognitivePriority::Reasoning
    }

    fn on_start(&mut self, ctx: &AgentContext) {
        if !self.greeted {
            self.print(ctx, concat!(
                "\n",
                "  +==================================+\n",
                "  |   Squirrel AIOS                  |\n",
                "  |   AI Sovereign Operating System  |\n",
                "  +==================================+\n",
                "\n",
                "  Type anything. Type 'help' for commands.\n",
                "\n",
                "> ",
            ).as_bytes());
            self.greeted = true;
        }

        self.glass_box(ctx, "state", "waiting");
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // Process all pending intents in the inbox
        while let Some(intent) = ctx.bus.try_recv() {
            let intent_type = intent.semantic_type.as_str();

            match (&self.state, intent_type) {
                // ── User typed a line ──────────────────────────────
                (AgentState::WaitingForInput, "input.line") => {
                    // Input module sends raw UTF-8 bytes (not postcard)
                    let user_input = match core::str::from_utf8(&intent.payload) {
                        Ok(s) => s.trim(),
                        Err(_) => return AgentPoll::Yield,
                    };

                    if user_input.is_empty() {
                        self.print(ctx, b"> ");
                        return AgentPoll::Yield;
                    }

                    // Update Glass Box with the user's input
                    let display_input: String = user_input.chars().take(40).collect();
                    self.glass_box(ctx, "last_input", &display_input);

                    // Try pattern matching first (instant, no AI needed)
                    if let Some(plan) = try_pattern_match(user_input) {
                        for step in plan.steps {
                            self.send_raw(ctx, &step.intent_type, &step.payload);
                        }
                        // For settings/modules, wait for them to close
                        if user_input.trim().eq_ignore_ascii_case("settings")
                            || user_input.contains("configure")
                            || user_input.contains("api key")
                        {
                            self.state = AgentState::WaitingForModule;
                            self.glass_box(ctx, "state", "module-active");
                        } else {
                            // For instant responses (help, status), show prompt
                            self.print(ctx, b"> ");
                        }
                        return AgentPoll::Yield;
                    }

                    // No pattern match — send to AI inference engine
                    let request = build_inference_request(user_input);
                    let infer_intent = Intent::request(
                        "inference.generate",
                        "primary-agent",
                        &request,
                    );
                    ctx.bus.send(infer_intent);

                    self.state = AgentState::WaitingForInference;
                    self.glass_box(ctx, "state", "thinking");
                    self.print(ctx, b"[thinking...]\n");

                    return AgentPoll::Yield;
                }

                // ── AI inference response ─────────────────────────
                (AgentState::WaitingForInference, t) if t.starts_with("inference.generate") => {
                    // Try to decode as InferenceResponse (postcard-encoded)
                    if let Ok(response) = intent.decode::<InferenceResponse>() {
                        let output = format!("{}\n", response.text);
                        self.print(ctx, output.as_bytes());

                        let stats = format!(
                            "[{} in {}ms]\n> ",
                            response.backend_used, response.latency_ms
                        );
                        self.print(ctx, stats.as_bytes());
                    } else if let Ok(error_text) = intent.decode::<String>() {
                        // Error response (router sends error as String)
                        let msg = format!("[Error: {}]\n> ", error_text);
                        self.print(ctx, msg.as_bytes());
                    } else {
                        self.print(ctx, b"[Error: could not decode response]\n> ");
                    }

                    self.state = AgentState::WaitingForInput;
                    self.glass_box(ctx, "state", "waiting");
                    return AgentPoll::Yield;
                }

                // ── Module closed (settings, etc.) ────────────────
                (AgentState::WaitingForModule, "settings.closed")
                | (AgentState::WaitingForModule, "display.clear.done") => {
                    self.print(ctx, b"\n> ");
                    self.state = AgentState::WaitingForInput;
                    self.glass_box(ctx, "state", "waiting");
                    return AgentPoll::Yield;
                }

                // ── System status request ─────────────────────────
                (_, "system.status") => {
                    let backend = settings::current_backend();
                    let status = format!(
                        "Squirrel AIOS Status:\n  Backend: {}\n  Tick: {}\n> ",
                        backend, ctx.tick
                    );
                    self.print(ctx, status.as_bytes());
                    return AgentPoll::Yield;
                }

                // Ignore everything else
                _ => {}
            }
        }

        AgentPoll::Pending
    }
}
