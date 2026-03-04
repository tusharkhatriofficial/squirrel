//! Hello Module — the first WASM capability module for Squirrel AIOS.
//!
//! This is compiled to WebAssembly (wasm32-unknown-unknown) and embedded
//! in the kernel. When loaded, it:
//!   1. Logs "Hello from WASM!" to the kernel console
//!   2. Sends a "hello.world" intent through the Intent Bus
//!
//! It proves that the entire WASM pipeline works:
//!   Rust source → WASM binary → wasmi interpreter → host ABI → Intent Bus
//!
//! The module imports 5 functions from the "squirrel" namespace (the host ABI),
//! though it only uses two: log() and intent_send().

#![no_std]

// Import the host functions provided by the kernel's WASM runtime.
// These are the ONLY way this module can interact with the outside world.
#[link(wasm_import_module = "squirrel")]
extern "C" {
    /// Send an intent: (type_ptr, type_len, payload_ptr, payload_len) -> status
    fn intent_send(type_ptr: *const u8, type_len: i32, payload_ptr: *const u8, payload_len: i32) -> i32;
    /// Receive an intent: (buf_ptr, buf_len) -> bytes_written or 0
    fn intent_recv(buf_ptr: *mut u8, buf_len: i32) -> i32;
    /// Update Glass Box state: (key_ptr, key_len, val_ptr, val_len)
    fn glass_box_update(key_ptr: *const u8, key_len: i32, val_ptr: *const u8, val_len: i32);
    /// Get nanoseconds since boot
    fn time_ns() -> i64;
    /// Log a message to the kernel console
    fn log(msg_ptr: *const u8, msg_len: i32);
}

/// Called once when the module is first loaded by SART.
/// This is the module's chance to do setup, send greetings, etc.
#[no_mangle]
pub extern "C" fn init() {
    // Log a message — this will appear as "[wasm/hello-module] Hello from WASM!"
    let msg = b"Hello from WASM!";
    unsafe {
        log(msg.as_ptr(), msg.len() as i32);
    }

    // Send a "hello.world" intent through the Intent Bus.
    // Any agent subscribed to "hello" or "hello.world" will receive it.
    let intent_type = b"hello.world";
    let payload = b"\x00"; // minimal postcard-encoded empty payload
    unsafe {
        intent_send(
            intent_type.as_ptr(),
            intent_type.len() as i32,
            payload.as_ptr(),
            payload.len() as i32,
        );
    }
}

/// Called every scheduler tick (100 Hz).
/// Hello-module is a one-shot module — it does all its work in init().
#[no_mangle]
pub extern "C" fn poll() {
    // Nothing to do each tick — the hello module only runs once.
}

/// Panic handler for no_std WASM — just halts (wasmi will catch the trap).
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
