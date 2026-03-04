//! Heartbeat agent — proves SART is scheduling and the Intent Bus is routing.
//!
//! Prints a heartbeat message every 100 ticks (1 second at 100 Hz) and sends
//! a "system.heartbeat" intent for other agents to consume.

use crate::println;
use intent_bus::Intent;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Heartbeat {
    pub beat: u32,
    pub tick: u64,
}

pub struct HeartbeatAgent {
    last_tick: u64,
    beat_count: u32,
}

impl HeartbeatAgent {
    pub fn new() -> Self {
        Self {
            last_tick: 0,
            beat_count: 0,
        }
    }
}

impl Agent for HeartbeatAgent {
    fn name(&self) -> &str {
        "heartbeat"
    }

    fn priority(&self) -> CognitivePriority {
        CognitivePriority::Background
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // Fire every 100 ticks (1 second at 100 Hz)
        if ctx.tick >= self.last_tick + 100 {
            self.beat_count += 1;
            println!(
                "[heartbeat] tick={} beat={}",
                ctx.tick, self.beat_count
            );

            let intent = Intent::request(
                "system.heartbeat",
                "heartbeat",
                &Heartbeat {
                    beat: self.beat_count,
                    tick: ctx.tick,
                },
            );
            ctx.bus.send(intent);

            self.last_tick = ctx.tick;
        }
        AgentPoll::Pending
    }
}
