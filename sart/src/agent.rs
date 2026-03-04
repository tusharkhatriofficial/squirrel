//! Agent abstraction — the fundamental execution unit in Squirrel AIOS.
//!
//! Every AI agent, capability module, and system service implements the Agent
//! trait. SART schedules agents cooperatively: each agent is polled once per
//! tick in priority order and must return quickly without blocking.
//!
//! Agents communicate exclusively through the Intent Bus — they receive intents
//! via their BusConnection and send intents back. This makes all agent behavior
//! observable through the Glass Box audit log.

use alloc::boxed::Box;
use intent_bus::BusConnection;

/// Priority of an agent — determines scheduling order.
/// Higher priority agents are polled first each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CognitivePriority {
    Background = 0, // Rarely polled (e.g., cleanup, metrics)
    Waiting = 1,    // Blocked on I/O or external events
    Active = 2,     // Active worker (e.g., network agent)
    Reasoning = 3,  // High-priority reasoning agent (Primary AI)
}

/// Result of one poll of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPoll {
    /// Agent did useful work — poll again next tick.
    Yield,
    /// Agent has nothing to do — skip until next tick (SART may reduce
    /// poll frequency for persistently idle agents).
    Pending,
    /// Agent is finished and should be removed from the scheduler.
    Done,
}

/// Context passed to an agent each time it is polled.
pub struct AgentContext<'a> {
    /// The agent's bus connection for sending/receiving intents.
    pub bus: &'a BusConnection,
    /// Current tick count (100 Hz — 10ms per tick).
    pub tick: u64,
}

/// The core Agent trait — all agents implement this.
///
/// Agents are cooperative: `poll()` must return quickly without blocking.
/// Long-running work should be broken into small steps across multiple polls.
/// Inter-agent communication goes through the Intent Bus (via `ctx.bus`).
pub trait Agent: Send {
    /// Unique name for this agent (used for bus routing and display).
    fn name(&self) -> &str;

    /// Priority — determines scheduling order (higher = polled first).
    fn priority(&self) -> CognitivePriority;

    /// Called once when the agent is first registered with SART.
    fn on_start(&mut self, _ctx: &AgentContext) {}

    /// Called every scheduler tick. Do one unit of work and return.
    /// Must NOT block — use AgentPoll::Pending if there's nothing to do.
    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll;

    /// Called when the agent is removed from the scheduler.
    fn on_stop(&mut self) {}
}

/// A boxed, heap-allocated agent (what SART stores internally).
pub type BoxedAgent = Box<dyn Agent>;
