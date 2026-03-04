//! GlassBoxAgent — the SART agent that manages the Glass Box.
//!
//! In Squirrel AIOS, the Glass Box itself runs as an agent. This is the
//! Squirrel philosophy: everything, even system infrastructure, is an agent
//! that communicates through the Intent Bus.
//!
//! The GlassBoxAgent does two things:
//!
//! 1. RECEIVES state updates: Other agents send "glass-box.update" intents
//!    when they want to publish state. The GlassBoxAgent decodes these
//!    intents and writes the key-value pairs into the GlassBoxStore.
//!
//! 2. RENDERS the display: Every 50 ticks (500ms), the agent reads all
//!    module snapshots from the store, renders them as an ASCII box, and
//!    prints the result to the framebuffer.
//!
//! Why use intents instead of direct store writes?
//! Because intents go through the Intent Bus audit log. This means the
//! Glass Box's OWN state updates are visible in the Glass Box — it's
//! self-observing. Also, intents can be routed, filtered, and replayed,
//! which matters for debugging.
//!
//! Note: WASM modules update the store directly (via the glass_box_update
//! host function) for performance. The intent path is for Rust agents.

use alloc::string::String;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use serde::{Deserialize, Serialize};

/// Payload for the "glass-box.update" intent.
///
/// When an agent wants to publish state to the Glass Box, it creates an
/// Intent with this payload. The GlassBoxAgent decodes it and calls
/// GLASS_BOX.update(module, key, value).
///
/// Example:
/// ```ignore
/// let intent = Intent::request("glass-box.update", "heartbeat",
///     &GlassBoxUpdate {
///         module: "heartbeat".into(),
///         key: "beat_count".into(),
///         value: "5".into(),
///     }
/// );
/// ctx.bus.send(intent);
/// ```
#[derive(Serialize, Deserialize, Debug)]
pub struct GlassBoxUpdate {
    /// Which module this update is for (e.g., "heartbeat")
    pub module: String,
    /// The state key (e.g., "beat_count")
    pub key: String,
    /// The state value (e.g., "5")
    pub value: String,
}

/// The Glass Box SART agent.
///
/// Subscribes to "glass-box.update" and "glass-box.module.stopped" intents.
/// Periodically renders the Glass Box overlay to the framebuffer.
///
/// `display_enabled` controls whether rendering happens. Set to false during
/// development if the output is too noisy alongside other kernel messages.
pub struct GlassBoxAgent {
    /// Tick count at which we last rendered the display
    render_tick: u64,
    /// Whether to periodically render the Glass Box overlay
    display_enabled: bool,
}

impl GlassBoxAgent {
    /// Create a new GlassBoxAgent.
    ///
    /// `display_enabled`: if true, the agent renders the Glass Box overlay
    /// every 50 ticks (500ms). If false, the agent still processes intents
    /// and updates the store, but doesn't render.
    pub fn new(display_enabled: bool) -> Self {
        Self {
            render_tick: 0,
            display_enabled,
        }
    }
}

impl Agent for GlassBoxAgent {
    fn name(&self) -> &str {
        "glass-box"
    }

    fn priority(&self) -> CognitivePriority {
        // Background priority — the Glass Box is observational, not critical.
        // It should never preempt real work like the AI reasoning agent or
        // capability modules. It just watches and renders.
        CognitivePriority::Background
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        let mut did_work = false;

        // ── Step 1: Process all pending Glass Box intents ──────────────
        //
        // Other agents send intents like:
        //   "glass-box.update" → { module: "heartbeat", key: "count", value: "5" }
        //   "glass-box.module.stopped" → "heartbeat" (the module name as a string)
        //
        // We drain the inbox completely each tick to avoid a backlog.
        while let Some(intent) = ctx.bus.try_recv() {
            match intent.semantic_type.as_str() {
                // A module is publishing a state update
                "glass-box.update" => {
                    if let Ok(update) = intent.decode::<GlassBoxUpdate>() {
                        crate::GLASS_BOX.update(&update.module, &update.key, &update.value);
                        did_work = true;
                    }
                }
                // A module has stopped — remove it from the Glass Box
                "glass-box.module.stopped" => {
                    if let Ok(name) = intent.decode::<String>() {
                        crate::GLASS_BOX.remove(&name);
                        did_work = true;
                    }
                }
                _ => {}
            }
        }

        // ── Step 2: Render the Glass Box overlay periodically ──────────
        //
        // Every 50 ticks (500ms at 100Hz), we:
        // 1. Read all module snapshots from the store
        // 2. Render them as an ASCII box using display::render_to_string()
        // 3. Print the result to the framebuffer via the log bridge
        //
        // 500ms is fast enough to feel "real-time" but slow enough to not
        // flood the framebuffer with constant redraws.
        if self.display_enabled && ctx.tick >= self.render_tick + 50 {
            let snapshots = crate::GLASS_BOX.snapshot();
            if !snapshots.is_empty() {
                let rendered = crate::display::render_to_string(&snapshots, 20);
                crate::log_msg(&rendered);
            }
            self.render_tick = ctx.tick;
            did_work = true;
        }

        // Return Yield if we did work (tells SART to poll us again soon)
        // or Pending if we had nothing to do (SART may reduce our frequency)
        if did_work {
            AgentPoll::Yield
        } else {
            AgentPoll::Pending
        }
    }
}
