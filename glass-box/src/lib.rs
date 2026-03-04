//! Glass Box — real-time state visibility for Squirrel AIOS.
//!
//! The Glass Box is one of Squirrel's six foundational principles. It means:
//! every running process (agent, WASM module, kernel service) publishes its
//! internal state to a shared, always-visible store. The AI can see inside
//! any process at any moment without stopping it.
//!
//! This crate provides:
//! - `GLASS_BOX` — the global state store (GlassBoxStore)
//! - `update()` — convenience function to publish state from anywhere
//! - `GlassBoxAgent` — SART agent that processes intents and renders display
//! - `GlassBoxUpdate` — the intent payload format for state updates
//! - `render_to_string()` — ASCII box renderer for the framebuffer
//!
//! Usage from any kernel code:
//! ```ignore
//! glass_box::update("my-module", "status", "running");
//! ```
//!
//! Usage from WASM modules:
//! WASM modules call the `glass_box_update` host function, which writes
//! directly to GLASS_BOX without going through intents (for performance).

#![no_std]
extern crate alloc;

pub mod state;
pub mod display;
pub mod agent;

// Re-export the main types so users can write `glass_box::GlassBoxStore`
// instead of `glass_box::state::GlassBoxStore`.
pub use state::{GlassBoxStore, ModuleSnapshot, StateValue};
pub use agent::{GlassBoxAgent, GlassBoxUpdate};

// ── Global Glass Box instance ──────────────────────────────────────────
//
// There's exactly one GlassBoxStore in the entire kernel. It's a static
// so it exists for the lifetime of the OS. The `const fn new()` means
// no runtime initialization is needed — it's ready at boot.

/// The global Glass Box state store.
///
/// All agents and WASM modules write to this single instance. The
/// GlassBoxAgent reads from it to render the display overlay.
pub static GLASS_BOX: GlassBoxStore = GlassBoxStore::new();

/// Convenience function: update a state entry from anywhere in the kernel.
///
/// This is the fastest way to publish state — it writes directly to the
/// store without going through the Intent Bus. Use this from kernel code
/// and WASM host functions. For SART agents that want audit visibility,
/// send a "glass-box.update" intent instead.
///
/// Example: `glass_box::update("kernel", "status", "booting")`
pub fn update(module: &str, key: &str, value: &str) {
    GLASS_BOX.update(module, key, value);
}

// ── Log bridge ─────────────────────────────────────────────────────────
//
// The glass-box crate is a library — it can't use the kernel's println!
// macro directly (that macro is defined in the kernel crate). Instead,
// the kernel registers a function pointer at boot time using set_log_fn().
// When the GlassBoxAgent needs to print the rendered overlay, it calls
// log_msg() which forwards to that function pointer.
//
// This is the same pattern used by wasm-runtime for WASM module logging.

use core::sync::atomic::{AtomicUsize, Ordering};

type LogFn = fn(&str);

/// Stores the kernel's print function as an atomic usize (function pointer).
static LOG_FN: AtomicUsize = AtomicUsize::new(0);

/// Register the log function. Called once by the kernel during init.
///
/// After this, the GlassBoxAgent can print to the framebuffer by calling
/// log_msg(). Without this call, log_msg() silently discards output.
pub fn set_log_fn(f: LogFn) {
    LOG_FN.store(f as usize, Ordering::Release);
}

/// Internal: send a message to the kernel's framebuffer (if log_fn is set).
///
/// Used by the GlassBoxAgent to print the rendered ASCII overlay.
pub(crate) fn log_msg(msg: &str) {
    let addr = LOG_FN.load(Ordering::Acquire);
    if addr != 0 {
        let f: LogFn = unsafe { core::mem::transmute(addr) };
        f(msg);
    }
}
