//! GlassBoxStore — the central state store for the Glass Box visibility system.
//!
//! Every running agent and WASM module in Squirrel publishes key-value state
//! entries into the Glass Box. This module stores those entries in a per-module
//! map. The kernel renders this state as an overlay so you can see what every
//! component is doing in real time.
//!
//! How it works:
//! - Each module (agent or WASM capability) gets its own "snapshot" — a named
//!   bag of key-value pairs like { "status": "running", "beat_count": "5" }.
//! - When a module calls `update("heartbeat", "beat_count", "5")`, we store
//!   that key-value pair in the heartbeat module's snapshot.
//! - Each module can have at most 32 entries. If a 33rd key is added, the
//!   oldest entry (by timestamp) is evicted to make room.
//! - The store is protected by a spin::RwLock so multiple agents can read
//!   snapshots while one agent writes updates, without blocking each other.
//!
//! The store is purely in-RAM — state is lost on reboot. This is intentional:
//! the Glass Box shows CURRENT runtime state, not history. If you need history,
//! write to SVFS instead.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::RwLock;

/// Maximum length for a state entry key (longer keys are silently truncated).
pub const MAX_KEY_LEN: usize = 64;
/// Maximum length for a state entry value (longer values are silently truncated).
pub const MAX_VAL_LEN: usize = 256;
/// Maximum number of key-value entries per module. When this limit is reached,
/// the oldest entry is evicted to make room for a new key.
pub const MAX_ENTRIES_PER_MODULE: usize = 32;

/// A snapshot of one module's state at a point in time.
///
/// Each running agent or WASM module gets one of these. It contains:
/// - The module's name (e.g., "heartbeat", "hello-module")
/// - A BTreeMap of key-value pairs (the module's published state)
/// - The timestamp of the most recent update
/// - Whether the module is currently active (running) or inactive (stopped)
#[derive(Debug, Clone)]
pub struct ModuleSnapshot {
    /// Module name — must match the agent's name() or WASM module name.
    pub name: String,
    /// Key-value state pairs. Each key maps to a StateValue which includes
    /// the value string and the timestamp when it was last updated.
    pub state: BTreeMap<String, StateValue>,
    /// Timestamp (ms since boot) of the most recent update to any key.
    pub last_update_ms: u64,
    /// True if the module is currently running. False if it has been stopped
    /// but we're still showing its last-known state for a while.
    pub is_active: bool,
}

/// A single state value with its update timestamp.
///
/// We store the timestamp per-value so we can evict the OLDEST entry
/// when a module exceeds MAX_ENTRIES_PER_MODULE keys.
#[derive(Debug, Clone)]
pub struct StateValue {
    /// The value as a string (e.g., "42", "running", "hello world").
    pub value: String,
    /// When this value was last written (ms since boot).
    pub updated_at_ms: u64,
}

/// The Glass Box state store — holds per-module state snapshots.
///
/// This is the core data structure. There's one global instance (GLASS_BOX)
/// that all agents and WASM modules write to. The GlassBoxAgent reads from
/// it periodically to render the display overlay.
///
/// Thread safety: protected by spin::RwLock. Multiple readers can read
/// snapshots concurrently (for rendering), while writes (state updates)
/// take an exclusive lock briefly.
pub struct GlassBoxStore {
    modules: RwLock<BTreeMap<String, ModuleSnapshot>>,
}

impl GlassBoxStore {
    /// Create a new, empty Glass Box store.
    ///
    /// This is const so we can create the global GLASS_BOX static at compile
    /// time — no initialization function needed at runtime.
    pub const fn new() -> Self {
        Self {
            modules: RwLock::new(BTreeMap::new()),
        }
    }

    /// Update a single state key for a module.
    ///
    /// If the module doesn't exist yet, it's created automatically (marked
    /// as active). If the module already has MAX_ENTRIES_PER_MODULE keys and
    /// this is a NEW key, the oldest entry (by timestamp) is evicted first.
    ///
    /// Example: `store.update("heartbeat", "beat_count", "5")`
    /// This sets heartbeat's "beat_count" to "5" with the current timestamp.
    pub fn update(&self, module: &str, key: &str, value: &str) {
        let now_ms = current_ms();
        let mut modules = self.modules.write();

        // Get or create the module's snapshot
        let snapshot = modules
            .entry(String::from(module))
            .or_insert_with(|| ModuleSnapshot {
                name: String::from(module),
                state: BTreeMap::new(),
                last_update_ms: now_ms,
                is_active: true,
            });

        // If we're at the entry limit and this is a new key, evict the oldest
        if snapshot.state.len() >= MAX_ENTRIES_PER_MODULE
            && !snapshot.state.contains_key(key)
        {
            // Find the key with the smallest (oldest) timestamp
            if let Some(oldest_key) = snapshot
                .state
                .iter()
                .min_by_key(|(_, v)| v.updated_at_ms)
                .map(|(k, _)| k.clone())
            {
                snapshot.state.remove(&oldest_key);
            }
        }

        // Truncate value if too long (prevents a buggy module from eating
        // all our heap memory with a giant string)
        let truncated_value: String = value.chars().take(MAX_VAL_LEN).collect();

        // Insert or update the key
        snapshot.state.insert(
            String::from(key),
            StateValue {
                value: truncated_value,
                updated_at_ms: now_ms,
            },
        );
        snapshot.last_update_ms = now_ms;
    }

    /// Mark a module as active or inactive.
    ///
    /// Inactive modules are shown with an "○" in the display instead of "●".
    /// This is called when an agent stops but we still want to show its
    /// last-known state for a while before removing it.
    pub fn set_active(&self, module: &str, active: bool) {
        let mut modules = self.modules.write();
        if let Some(snap) = modules.get_mut(module) {
            snap.is_active = active;
        }
    }

    /// Get a snapshot of ALL module states (for rendering the display overlay).
    ///
    /// Returns a Vec of cloned ModuleSnapshots. The clone is intentional:
    /// we release the read lock immediately so agents can keep writing
    /// while the renderer formats the output string.
    pub fn snapshot(&self) -> Vec<ModuleSnapshot> {
        self.modules.read().values().cloned().collect()
    }

    /// Get the state for a single module by name.
    ///
    /// Returns None if no module with that name has published state yet.
    pub fn get_module(&self, name: &str) -> Option<ModuleSnapshot> {
        self.modules.read().get(name).cloned()
    }

    /// Remove a module entirely from the store.
    ///
    /// Called when an agent is permanently removed from SART. After this,
    /// the module won't appear in the Glass Box display at all.
    pub fn remove(&self, module: &str) {
        self.modules.write().remove(module);
    }

    /// How many modules are currently tracked in the Glass Box.
    pub fn module_count(&self) -> usize {
        self.modules.read().len()
    }
}

/// Get current milliseconds since boot.
///
/// We reuse the Intent Bus time source rather than adding our own timer
/// dependency. The kernel updates this in its main loop, so it's accurate
/// to within one tick (10ms at 100 Hz).
fn current_ms() -> u64 {
    intent_bus::bus::current_ms()
}
