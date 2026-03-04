#![no_std]
extern crate alloc;

pub mod host_abi;
pub mod runtime;
pub mod module_agent;

pub use runtime::{WasmError, WasmModule};
pub use module_agent::ModuleAgent;

use core::sync::atomic::{AtomicUsize, Ordering};

// ── Kernel log bridge ──────────────────────────────────────────────
// wasm-runtime is a library crate — it can't use the kernel's println!
// macro directly. Instead, the kernel registers a function pointer at
// boot time, and we call through it whenever a WASM module logs.

type LogFn = fn(&str);

static LOG_FN: AtomicUsize = AtomicUsize::new(0);

/// Set the log function. Called once by the kernel during init.
/// After this, any WASM module calling the `log` host function will
/// have its output forwarded to this function.
pub fn set_log_fn(f: LogFn) {
    LOG_FN.store(f as usize, Ordering::Release);
}

/// Internal: call the registered log function (if any).
pub(crate) fn log_msg(msg: &str) {
    let addr = LOG_FN.load(Ordering::Acquire);
    if addr != 0 {
        let f: LogFn = unsafe { core::mem::transmute(addr) };
        f(msg);
    }
}
