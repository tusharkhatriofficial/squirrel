//! Echo agent — receives heartbeat intents and prints confirmation.
//!
//! Proves that inter-agent communication works: the heartbeat agent sends
//! a "system.heartbeat" intent, and this agent receives it via the Intent Bus.

use crate::println;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};

use super::heartbeat::Heartbeat;

pub struct EchoAgent;

impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn priority(&self) -> CognitivePriority {
        CognitivePriority::Active
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        match ctx.bus.try_recv() {
            Some(intent) => {
                if let Ok(_hb) = intent.decode::<Heartbeat>() {
                    // Heartbeat received — Intent Bus routing works.
                    // (Silent in production; Glass Box tracks state.)
                }
                AgentPoll::Yield
            }
            None => AgentPoll::Pending,
        }
    }
}
