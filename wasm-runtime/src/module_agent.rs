//! ModuleAgent — wraps a running WASM module instance as a SART agent.
//!
//! This is where the Capability Fabric meets the Agent Runtime. Each WASM
//! module becomes a first-class agent that SART schedules alongside native
//! Rust agents. The ModuleAgent handles:
//!
//! - Calling the WASM module's `poll()` export on each scheduler tick
//! - Delivering received intents from the bus into the WASM's HostState
//! - Dispatching outbound intents from the WASM module onto the bus
//!
//! From SART's perspective, a ModuleAgent looks identical to a native agent.
//! This is the key insight: WASM modules are not second-class citizens.

use wasmi::{AsContextMut, Instance, Store, TypedFunc};
use alloc::{string::String, vec::Vec};
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use intent_bus::{Intent, IntentPriority, SemanticType};

use crate::host_abi::HostState;

/// A SART agent wrapping a live WASM module instance.
///
/// The WASM module's `init()` is called on agent start.
/// Its `poll()` is called every scheduler tick.
/// Intents flow through the HostState bridge.
pub struct ModuleAgent {
    /// Module name (for logging and bus routing)
    name: String,
    /// Intent types this module subscribes to
    subscriptions: Vec<String>,
    /// The wasmi store holding the module's memory and HostState
    store: Store<HostState>,
    /// The instantiated WASM module
    instance: Instance,
    /// Cached reference to the WASM `poll()` export (avoids lookup each tick)
    poll_fn: Option<TypedFunc<(), ()>>,
}

impl ModuleAgent {
    pub fn new(
        name: String,
        subscriptions: Vec<String>,
        store: Store<HostState>,
        instance: Instance,
    ) -> Self {
        Self {
            name,
            subscriptions,
            store,
            instance,
            poll_fn: None,
        }
    }

    /// Get the intent types this module subscribes to.
    pub fn subscriptions(&self) -> Vec<&str> {
        self.subscriptions.iter().map(|s| s.as_str()).collect()
    }
}

impl Agent for ModuleAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> CognitivePriority {
        // WASM capability modules run at Active priority — they're doing
        // real work (display, input, storage), not just background tasks.
        CognitivePriority::Active
    }

    fn on_start(&mut self, _ctx: &AgentContext) {
        // Cache the poll() function export for fast access on each tick.
        // Not all modules export poll() — some only have init().
        self.poll_fn = self
            .instance
            .get_typed_func::<(), ()>(&self.store, "poll")
            .ok();

        // Call the WASM module's init() export if it has one.
        // This is where modules do one-time setup: register subscriptions,
        // send initial intents, log startup messages, etc.
        if let Ok(init_fn) = self
            .instance
            .get_typed_func::<(), ()>(&self.store, "init")
        {
            if let Err(e) = init_fn.call(&mut self.store, ()) {
                let msg = alloc::format!("[wasm/{}] init() failed: {:?}", self.name, e);
                crate::log_msg(&msg);
            }
        }

        let msg = alloc::format!("[wasm/{}] started", self.name);
        crate::log_msg(&msg);
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // ── Step 1: Deliver any pending intent from the bus to the module ──
        // The SART scheduler gave us a BusConnection (ctx.bus). If there's
        // an intent waiting, put it in HostState so the WASM module can
        // read it via the intent_recv() host function.
        if let Some(intent) = ctx.bus.try_recv() {
            self.store.as_context_mut().data_mut().pending_recv = Some(intent);
        }

        // ── Step 2: Call the WASM module's poll() function ──
        // This is where the module does its work. It might call
        // intent_send(), log(), glass_box_update(), etc.
        if let Some(poll_fn) = self.poll_fn {
            match poll_fn.call(&mut self.store, ()) {
                Ok(()) => {}
                Err(e) => {
                    let msg = alloc::format!("[wasm/{}] poll() error: {:?}", self.name, e);
                    crate::log_msg(&msg);
                    return AgentPoll::Done; // fatal — remove agent
                }
            }
        }

        // ── Step 3: Dispatch ALL outbound intents from the module ──
        // If the WASM module called intent_send() multiple times during
        // its poll(), all intents are queued in HostState.pending_send.
        // We drain them all and send each onto the real Intent Bus.
        let pending = core::mem::take(&mut self.store.as_context_mut().data_mut().pending_send);
        for (intent_type, payload) in pending {
            let intent = Intent {
                id: 0, // assigned by the bus
                reply_to: None,
                semantic_type: SemanticType::new(&intent_type),
                sender: self.name.clone(),
                payload,
                priority: IntentPriority::Normal,
                timestamp_ms: 0, // set by the bus
            };
            ctx.bus.send(intent);
        }

        AgentPoll::Pending
    }

    fn on_stop(&mut self) {
        let msg = alloc::format!("[wasm/{}] stopped", self.name);
        crate::log_msg(&msg);
    }
}
