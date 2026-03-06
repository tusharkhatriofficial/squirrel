//! Planner — decides how to handle user input.
//!
//! Before sending anything to the AI inference engine (which costs time and
//! possibly money), the planner checks if the input matches a known pattern.
//! Simple commands like "help", "settings", "clear", "status" are handled
//! instantly without touching the AI at all.
//!
//! If no pattern matches, the planner builds an InferenceRequest with the
//! system prompt and user message, ready to send to the inference engine.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use inference_engine::InferenceRequest;

/// A plan: one or more steps to execute in response to user input.
/// Each step becomes an intent sent through the bus.
pub struct WorkflowPlan {
    pub steps: Vec<WorkflowStep>,
}

/// A single step in a workflow plan.
pub struct WorkflowStep {
    /// The intent type to send (e.g., "display.print", "settings.open")
    pub intent_type: String,
    /// Raw payload bytes for the intent
    pub payload: Vec<u8>,
}

impl WorkflowPlan {
    /// Create a plan with a single step.
    pub fn single(intent_type: &str, payload: &[u8]) -> Self {
        Self {
            steps: vec![WorkflowStep {
                intent_type: String::from(intent_type),
                payload: payload.to_vec(),
            }],
        }
    }
}

/// Try to match user input against known patterns.
///
/// Returns Some(plan) if the input is a recognized command,
/// None if it needs to be sent to the AI inference engine.
pub fn try_pattern_match(input: &str) -> Option<WorkflowPlan> {
    let lower = input.trim();

    // Settings
    if eq_ignore_case(lower, "settings")
        || contains_ignore_case(lower, "configure")
        || contains_ignore_case(lower, "change model")
        || contains_ignore_case(lower, "api key")
        || contains_ignore_case(lower, "switch to openai")
        || contains_ignore_case(lower, "switch to anthropic")
        || contains_ignore_case(lower, "switch to gemini")
        || contains_ignore_case(lower, "use local model")
    {
        return Some(WorkflowPlan::single("settings.open", b"{}"));
    }

    // Help
    if eq_ignore_case(lower, "help") || lower == "?" {
        let help = b"Squirrel AIOS \xe2\x80\x94 Commands:\n\
                      \x20 settings    \xe2\x80\x94 configure AI backend\n\
                      \x20 help        \xe2\x80\x94 show this help\n\
                      \x20 clear       \xe2\x80\x94 clear the screen\n\
                      \x20 status      \xe2\x80\x94 show system status\n";
        return Some(WorkflowPlan::single("display.print", help));
    }

    // Clear screen
    if eq_ignore_case(lower, "clear") || eq_ignore_case(lower, "cls") {
        return Some(WorkflowPlan::single("display.clear", b""));
    }

    // Status
    if eq_ignore_case(lower, "status") || eq_ignore_case(lower, "ps") || eq_ignore_case(lower, "agents") {
        return Some(WorkflowPlan::single("system.status", b""));
    }

    None
}

/// Build an InferenceRequest from user input (when pattern matching fails).
pub fn build_inference_request(user_input: &str) -> InferenceRequest {
    InferenceRequest {
        prompt: alloc::format!("{}\n\nUser: {}\nAssistant:", SYSTEM_PROMPT, user_input),
        max_tokens: 512,
        temperature: 0.7,
        stop_sequences: vec![String::from("User:"), String::from("\n\nUser")],
    }
}

/// The system prompt — tells the AI who it is.
pub const SYSTEM_PROMPT: &str = "\
You are Squirrel, an AI operating system. You are running on bare metal hardware. \
You have direct access to the system's hardware, memory, and all running processes. \
You are helpful, concise, and aware of the system state. \
Keep responses brief — this is a terminal interface.";

// Case-insensitive helpers (no allocator needed for comparison)
fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    for i in 0..=(h.len() - n.len()) {
        let mut matched = true;
        for j in 0..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j].to_ascii_lowercase() {
                matched = false;
                break;
            }
        }
        if matched {
            return true;
        }
    }
    false
}
