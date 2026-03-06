//! Storage module — exposes SVFS to capability modules via the Intent Bus.
//!
//! WASM modules cannot call SVFS directly (it lives in kernel space). Instead,
//! they send storage intents to this module, which forwards them to the kernel.
//! The kernel performs the actual SVFS operation and sends a response back,
//! which this module forwards to the original requester.
//!
//! This is the Capability Fabric in action: SVFS access is a capability that
//! any module can use, but the actual storage operations are controlled and
//! auditable through the Intent Bus.
//!
//! Subscribes to:
//!   "storage.store"                     — store data in SVFS
//!   "storage.retrieve"                  — retrieve data from SVFS
//!   "kernel.storage.store.response"     — kernel's response after storing
//!   "kernel.storage.retrieve.response"  — kernel's response after retrieval
//!
//! Sends:
//!   "storage.ready"              — emitted on init to signal readiness
//!   "kernel.storage.store"       — forwarded store request to kernel
//!   "kernel.storage.retrieve"    — forwarded retrieve request to kernel
//!   "storage.store.response"     — forwarded response back to requester
//!   "storage.retrieve.response"  — forwarded response back to requester

#![no_std]

// Host functions provided by the kernel's WASM runtime.
#[link(wasm_import_module = "squirrel")]
extern "C" {
    fn intent_send(
        type_ptr: *const u8,
        type_len: i32,
        payload_ptr: *const u8,
        payload_len: i32,
    ) -> i32;
    fn intent_recv(buf_ptr: *mut u8, buf_len: i32) -> i32;
    fn glass_box_update(
        key_ptr: *const u8,
        key_len: i32,
        val_ptr: *const u8,
        val_len: i32,
    );
}

/// Total storage operations performed (Glass Box metric).
static mut OPS_COUNT: u32 = 0;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn send_intent(intent_type: &[u8], payload: &[u8]) {
    unsafe {
        intent_send(
            intent_type.as_ptr(),
            intent_type.len() as i32,
            payload.as_ptr(),
            payload.len() as i32,
        );
    }
}

fn update_glass_box(key: &[u8], val: &[u8]) {
    unsafe {
        glass_box_update(
            key.as_ptr(),
            key.len() as i32,
            val.as_ptr(),
            val.len() as i32,
        );
    }
}

/// Convert a u32 to ASCII decimal in a fixed buffer. No allocator needed.
fn u32_to_ascii(mut n: u32, buf: &mut [u8; 10]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut i = 10;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let len = 10 - i;
    let mut j = 0;
    while j < len {
        buf[j] = buf[i + j];
        j += 1;
    }
    len
}

// ---------------------------------------------------------------------------
// Module lifecycle
// ---------------------------------------------------------------------------

/// Called once when the module is loaded by SART.
#[no_mangle]
pub extern "C" fn init() {
    send_intent(b"storage.ready", b"");
    update_glass_box(b"status", b"ready");
}

/// Called every scheduler tick (100 Hz).
///
/// Proxies storage requests between WASM modules and the kernel SVFS.
#[no_mangle]
pub extern "C" fn poll() {
    let mut buf = [0u8; 4096];
    let n = unsafe { intent_recv(buf.as_mut_ptr(), buf.len() as i32) };

    if n <= 0 {
        return;
    }

    // Parse intent wire format from host_abi:
    // [type_len: u16 LE][type_bytes][payload_len: u16 LE][payload_bytes]
    let n = n as usize;
    if n < 4 {
        return;
    }

    let type_len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    if 2 + type_len + 2 > n {
        return;
    }

    let intent_type = &buf[2..2 + type_len];
    let payload_len = u16::from_le_bytes([buf[2 + type_len], buf[2 + type_len + 1]]) as usize;
    let payload_start = 2 + type_len + 2;
    let payload_end = (payload_start + payload_len).min(n);
    let payload = &buf[payload_start..payload_end];

    if intent_type == b"storage.store" {
        // Forward store request to kernel
        send_intent(b"kernel.storage.store", payload);
        increment_ops();
    } else if intent_type == b"storage.retrieve" {
        // Forward retrieve request to kernel
        send_intent(b"kernel.storage.retrieve", payload);
        increment_ops();
    } else if intent_type == b"kernel.storage.store.response" {
        // Forward kernel's store response back to the requester
        send_intent(b"storage.store.response", payload);
    } else if intent_type == b"kernel.storage.retrieve.response" {
        // Forward kernel's retrieve response back to the requester
        send_intent(b"storage.retrieve.response", payload);
    }
}

fn increment_ops() {
    unsafe {
        OPS_COUNT += 1;
        let mut num_buf = [0u8; 10];
        let len = u32_to_ascii(OPS_COUNT, &mut num_buf);
        update_glass_box(b"ops", &num_buf[..len]);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
