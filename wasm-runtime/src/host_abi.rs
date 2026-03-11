//! The 5 host functions exposed to WASM modules.
//!
//! These are the ONLY kernel calls a capability module can make.
//! WASM modules import from module "squirrel" with these function names:
//!
//!   intent_send(type_ptr, type_len, payload_ptr, payload_len) -> i32
//!   intent_recv(buf_ptr, buf_len) -> i32
//!   glass_box_update(key_ptr, key_len, val_ptr, val_len)
//!   time_ns() -> i64
//!   log(msg_ptr, msg_len)
//!
//! This is the Capability Fabric's enforcement boundary. A WASM module
//! cannot access kernel memory, hardware, or other modules directly —
//! it can only communicate through these 5 functions + the Intent Bus.

use wasmi::{AsContext, AsContextMut, Caller, Linker};
use intent_bus::Intent;
use alloc::{string::String, vec::Vec};

/// State held per WASM module instance.
///
/// Each WASM module gets its own HostState in the wasmi Store.
/// The agent loop moves intents between HostState and the Intent Bus.
pub struct HostState {
    /// The module's name (used for bus routing and log prefixes)
    pub module_name: String,
    /// Pending outbound intents: (semantic_type, payload_bytes).
    /// Pushed by intent_send(), drained by the ModuleAgent's poll() loop.
    /// Vec instead of Option so multiple intents per tick aren't lost.
    pub pending_send: Vec<(String, Vec<u8>)>,
    /// Buffered received intent.
    /// Set by the ModuleAgent's poll() loop, consumed by intent_recv().
    pub pending_recv: Option<Intent>,
}

impl HostState {
    pub fn new(module_name: &str) -> Self {
        Self {
            module_name: String::from(module_name),
            pending_send: Vec::new(),
            pending_recv: None,
        }
    }
}

/// Register all 5 host functions into the wasmi linker.
///
/// The linker maps WASM import names ("squirrel"."intent_send", etc.)
/// to Rust closures. When a WASM module calls these imports, wasmi
/// invokes the corresponding closure with access to the module's
/// memory and HostState.
pub fn register_host_functions(linker: &mut Linker<HostState>) {
    // ── 1. intent_send ──────────────────────────────────────────────
    // WASM calls: intent_send(type_ptr, type_len, payload_ptr, payload_len) -> i32
    // Reads a semantic type string and payload bytes from WASM memory,
    // then stores them in HostState for the agent loop to dispatch.
    // Returns 0 on success, negative on error.
    linker
        .func_wrap(
            "squirrel",
            "intent_send",
            |mut caller: Caller<HostState>,
             type_ptr: i32,
             type_len: i32,
             payload_ptr: i32,
             payload_len: i32|
             -> i32 {
                // Get the WASM module's linear memory export
                let mem = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return -1,
                };

                // Read intent type and payload from WASM memory
                let type_bytes = {
                    let data = mem.data(caller.as_context());
                    read_wasm_bytes(data, type_ptr, type_len)
                };
                let payload_bytes = {
                    let data = mem.data(caller.as_context());
                    read_wasm_bytes(data, payload_ptr, payload_len)
                };

                // Parse the semantic type as UTF-8
                let intent_type = match core::str::from_utf8(&type_bytes) {
                    Ok(s) => String::from(s),
                    Err(_) => return -2,
                };

                // Store in HostState — the ModuleAgent will dispatch it
                let mut ctx = caller.as_context_mut();
                let state = ctx.data_mut();
                state.pending_send.push((intent_type, payload_bytes));
                0 // success
            },
        )
        .expect("failed to register intent_send");

    // ── 2. intent_recv ──────────────────────────────────────────────
    // WASM calls: intent_recv(buf_ptr, buf_len) -> i32
    // If an intent is pending, writes it to the WASM buffer as:
    //   [type_len: u16 LE][type_bytes][payload_len: u16 LE][payload_bytes]
    // Returns bytes written, 0 if no intent, or negative if buffer too small.
    linker
        .func_wrap(
            "squirrel",
            "intent_recv",
            |mut caller: Caller<HostState>, buf_ptr: i32, buf_len: i32| -> i32 {
                // Take the pending intent (if any) from HostState
                let pending = caller.as_context_mut().data_mut().pending_recv.take();
                let intent = match pending {
                    Some(i) => i,
                    None => return 0, // no intent pending
                };

                // Calculate how many bytes we need to write
                let type_bytes = intent.semantic_type.as_str().as_bytes();
                let payload = &intent.payload;
                let needed = 2 + type_bytes.len() + 2 + payload.len();

                let mem = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return -1,
                };

                let data = mem.data_mut(caller.as_context_mut());
                if buf_len as usize >= needed && (buf_ptr as usize) + needed <= data.len() {
                    let dst = &mut data[buf_ptr as usize..];
                    // Write type length (u16 LE)
                    let type_len_bytes = (type_bytes.len() as u16).to_le_bytes();
                    dst[0] = type_len_bytes[0];
                    dst[1] = type_len_bytes[1];
                    // Write type string
                    dst[2..2 + type_bytes.len()].copy_from_slice(type_bytes);
                    // Write payload length (u16 LE)
                    let offset = 2 + type_bytes.len();
                    let payload_len_bytes = (payload.len() as u16).to_le_bytes();
                    dst[offset] = payload_len_bytes[0];
                    dst[offset + 1] = payload_len_bytes[1];
                    // Write payload bytes
                    dst[offset + 2..offset + 2 + payload.len()].copy_from_slice(payload);
                    needed as i32
                } else {
                    // Buffer too small — return negative of required size
                    -(needed as i32)
                }
            },
        )
        .expect("failed to register intent_recv");

    // ── 3. glass_box_update ─────────────────────────────────────────
    // WASM calls: glass_box_update(key_ptr, key_len, val_ptr, val_len)
    // Updates the module's observable state in the Glass Box.
    //
    // This is the DIRECT path — WASM modules write straight to the
    // GlassBoxStore without going through intents (for performance).
    // The module name is automatically taken from the HostState, so a
    // WASM module can only update its own Glass Box entry.
    linker
        .func_wrap(
            "squirrel",
            "glass_box_update",
            |caller: Caller<HostState>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32| {
                let mem = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return,
                };
                let data = mem.data(caller.as_context());
                let key_bytes = read_wasm_bytes(data, key_ptr, key_len);
                let val_bytes = read_wasm_bytes(data, val_ptr, val_len);

                // Parse key and value as UTF-8 strings, then update the store
                if let (Ok(key_str), Ok(val_str)) = (
                    core::str::from_utf8(&key_bytes),
                    core::str::from_utf8(&val_bytes),
                ) {
                    let ctx = caller.as_context();
                    let module_name = &ctx.data().module_name;
                    glass_box::GLASS_BOX.update(module_name, key_str, val_str);
                }
            },
        )
        .expect("failed to register glass_box_update");

    // ── 4. time_ns ──────────────────────────────────────────────────
    // WASM calls: time_ns() -> i64
    // Returns approximate nanoseconds since boot.
    // Uses the Intent Bus time source (ms precision, converted to ns).
    linker
        .func_wrap(
            "squirrel",
            "time_ns",
            |_caller: Caller<HostState>| -> i64 {
                (intent_bus::bus::current_ms() * 1_000_000) as i64
            },
        )
        .expect("failed to register time_ns");

    // ── 5. log ──────────────────────────────────────────────────────
    // WASM calls: log(msg_ptr, msg_len)
    // Reads a UTF-8 string from WASM memory and forwards it to the
    // kernel's log output (via the function pointer set at init).
    linker
        .func_wrap(
            "squirrel",
            "log",
            |caller: Caller<HostState>, msg_ptr: i32, msg_len: i32| {
                let mem = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return,
                };
                let data = mem.data(caller.as_context());
                let msg = read_wasm_bytes(data, msg_ptr, msg_len);
                if let Ok(s) = core::str::from_utf8(&msg) {
                    let module_name = caller.as_context().data().module_name.clone();
                    let formatted = alloc::format!("[wasm/{}] {}", module_name, s);
                    crate::log_msg(&formatted);
                }
            },
        )
        .expect("failed to register log");

    // ── 6. display_write ─────────────────────────────────────────────
    // WASM calls: display_write(msg_ptr, msg_len)
    // Writes raw text directly to the framebuffer without any prefix or
    // trailing newline. Used by the display-module for character echo.
    linker
        .func_wrap(
            "squirrel",
            "display_write",
            |caller: Caller<HostState>, msg_ptr: i32, msg_len: i32| {
                let mem = match caller.get_export("memory") {
                    Some(wasmi::Extern::Memory(m)) => m,
                    _ => return,
                };
                let data = mem.data(caller.as_context());
                let msg = read_wasm_bytes(data, msg_ptr, msg_len);
                if let Ok(s) = core::str::from_utf8(&msg) {
                    extern "Rust" {
                        fn kernel_display_write(s: &str);
                    }
                    unsafe { kernel_display_write(s); }
                }
            },
        )
        .expect("failed to register display_write");
}

/// Safely read bytes from WASM linear memory.
///
/// If the pointer + length is out of bounds, returns an empty Vec
/// rather than panicking. This prevents a malicious or buggy WASM
/// module from crashing the kernel.
fn read_wasm_bytes(memory: &[u8], ptr: i32, len: i32) -> Vec<u8> {
    let ptr = ptr as usize;
    let len = len as usize;
    if ptr + len <= memory.len() {
        memory[ptr..ptr + len].to_vec()
    } else {
        Vec::new()
    }
}
