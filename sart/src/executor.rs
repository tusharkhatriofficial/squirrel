//! SART executor — cooperative, priority-based agent scheduler.
//!
//! The executor maintains a list of agents sorted by priority (highest first).
//! Each tick, it polls every agent once. Agents that return Done are removed.
//! Agents that are persistently idle (returning Pending) have their poll
//! frequency reduced to avoid wasting cycles.
//!
//! SART is driven from the kernel's main loop (not the timer ISR) to avoid
//! deadlock issues with heap allocation and mutex contention inside agents.

use alloc::string::ToString;
use alloc::vec::Vec;
use intent_bus::{BusConnection, INTENT_BUS};

use crate::agent::{AgentContext, AgentPoll, BoxedAgent};

/// Entry in the agent table.
struct AgentEntry {
    agent: BoxedAgent,
    bus: BusConnection,
    /// Consecutive Pending results — used to throttle idle agents.
    idle_count: u32,
}

/// The SART scheduler.
pub struct Sart {
    agents: Vec<AgentEntry>,
}

impl Sart {
    pub const fn new() -> Self {
        Self {
            agents: Vec::new(),
        }
    }

    /// Register an agent with the given bus subscriptions.
    /// Calls on_start() immediately, then inserts sorted by priority (highest first).
    pub fn register(
        &mut self,
        mut agent: BoxedAgent,
        subscriptions: &[&str],
        current_tick: u64,
    ) {
        let name = agent.name().to_string();
        let bus = INTENT_BUS.connect(&name, subscriptions);
        let ctx = AgentContext {
            bus: &bus,
            tick: current_tick,
        };
        agent.on_start(&ctx);

        self.agents.push(AgentEntry {
            agent,
            bus,
            idle_count: 0,
        });

        // Keep agents sorted by priority (highest first)
        self.agents
            .sort_by(|a, b| b.agent.priority().cmp(&a.agent.priority()));
    }

    /// Run one full scheduling round.
    ///
    /// Polls every agent once in priority order. Agents returning Done are
    /// removed. Persistently idle agents (>10 consecutive Pending) are skipped
    /// on odd ticks to reduce overhead.
    pub fn tick(&mut self, current_tick: u64) {
        let mut done_indices = Vec::new();

        for (i, entry) in self.agents.iter_mut().enumerate() {
            // Throttle persistently idle agents — skip on odd ticks
            if entry.idle_count > 10 && current_tick % 2 != 0 {
                continue;
            }

            let ctx = AgentContext {
                bus: &entry.bus,
                tick: current_tick,
            };

            match entry.agent.poll(&ctx) {
                AgentPoll::Yield => {
                    entry.idle_count = 0;
                }
                AgentPoll::Pending => {
                    entry.idle_count = entry.idle_count.saturating_add(1);
                }
                AgentPoll::Done => {
                    entry.agent.on_stop();
                    done_indices.push(i);
                }
            }
        }

        // Remove done agents (reverse order to preserve indices)
        for i in done_indices.into_iter().rev() {
            self.agents.remove(i);
        }
    }

    /// Number of currently registered agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Names of all registered agents (for display/debugging).
    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.iter().map(|e| e.agent.name()).collect()
    }
}
