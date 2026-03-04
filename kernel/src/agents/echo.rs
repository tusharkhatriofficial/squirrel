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
                if let Ok(hb) = intent.decode::<Heartbeat>() {
                    println!(
                        "[echo] received heartbeat #{} at tick {}",
                        hb.beat, hb.tick
                    );
                }
                AgentPoll::Yield
            }
            None => AgentPoll::Pending,
        }
    }
}
