//! Keyboard Bridge Agent — pumps keyboard characters into the Intent Bus.
//!
//! The PS/2 keyboard interrupt handler (IRQ 1) can't safely send intents
//! directly — it runs in ISR context where acquiring the Intent Bus locks
//! could deadlock. Instead, the ISR pushes decoded characters into a
//! VecDeque buffer (drivers::keyboard::INPUT_BUFFER).
//!
//! This agent runs in the normal SART scheduling loop (main context, not ISR).
//! Each tick, it drains the keyboard buffer and sends each character as an
//! "input.char" intent. The input-module WASM module receives these and
//! assembles them into complete lines.
//!
//! This is the bridge between hardware interrupts and the Intent Bus.

use alloc::vec;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use intent_bus::{Intent, IntentPriority, SemanticType};
use alloc::string::String;

/// The keyboard-to-intent bridge agent.
pub struct KeyboardBridgeAgent;

impl KeyboardBridgeAgent {
    pub fn new() -> Self {
        Self
    }
}

impl Agent for KeyboardBridgeAgent {
    fn name(&self) -> &str {
        "keyboard-bridge"
    }

    fn priority(&self) -> CognitivePriority {
        // High priority — keyboard input should be processed before
        // background tasks so the user sees immediate echo.
        CognitivePriority::Active
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        let mut had_input = false;

        // Drain all pending characters from the keyboard buffer.
        // try_read_char() uses without_interrupts() internally, so
        // it's safe to call from the main loop context.
        while let Some(ch) = crate::drivers::keyboard::try_read_char() {
            had_input = true;

            // Send each character as an "input.char" intent with the
            // raw byte as payload. The input-module receives this and
            // handles line editing, echo, etc.
            let intent = Intent {
                id: 0,
                reply_to: None,
                semantic_type: SemanticType::new("input.char"),
                sender: String::from("keyboard-bridge"),
                payload: vec![ch as u8],
                priority: IntentPriority::High,
                timestamp_ms: 0,
            };
            ctx.bus.send(intent);
        }

        if had_input {
            AgentPoll::Yield
        } else {
            AgentPoll::Pending
        }
    }
}
