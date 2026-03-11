//! Display module — owns the terminal/framebuffer output.
//!
//! Other modules and agents send intents to this module to render text
//! on the screen. This is the ONLY path to the display — all visual output
//! in Squirrel AIOS flows through here.
//!
//! Subscribes to:
//!   "display.print"  — render text payload to the framebuffer
//!   "display.clear"  — clear the screen
//!   "display.prompt" — print the AI prompt prefix "> "
//!
//! Sends:
//!   "display.ready"      — emitted on init to signal readiness
//!   "display.clear.done" — emitted after screen clear completes

#![no_std]

// Host functions provided by the kernel's WASM runtime.
// These are the ONLY way this module can interact with the outside world.
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
    fn log(msg_ptr: *const u8, msg_len: i32);
    fn display_write(msg_ptr: *const u8, msg_len: i32);
}

/// Number of lines printed since boot (tracked for Glass Box visibility).
static mut LINE_COUNT: u32 = 0;

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

/// Convert a u32 to ASCII decimal in a fixed buffer. Returns the slice of
/// valid digits. No allocator needed.
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
    // Shift digits to the front
    let len = 10 - i;
    let mut j = 0;
    while j < len {
        buf[j] = buf[i + j];
        j += 1;
    }
    len
}

// ---------------------------------------------------------------------------
// Module lifecycle (called by SART via WASM runtime)
// ---------------------------------------------------------------------------

/// Called once when the module is loaded.
#[no_mangle]
pub extern "C" fn init() {
    send_intent(b"display.ready", b"");
    update_glass_box(b"status", b"ready");
}

/// Called every scheduler tick (100 Hz).
///
/// Checks for incoming intents. When "display.print" arrives, the payload
/// text is forwarded to the kernel framebuffer via the log() host call.
#[no_mangle]
pub extern "C" fn poll() {
    let mut buf = [0u8; 2048];
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

    if intent_type == b"display.print" {
        // Write raw text to framebuffer (no prefix, no newline)
        unsafe {
            display_write(payload.as_ptr(), payload.len() as i32);
            LINE_COUNT += 1;
        }

        // Update Glass Box with line count
        let mut num_buf = [0u8; 10];
        let len = u32_to_ascii(unsafe { LINE_COUNT }, &mut num_buf);
        update_glass_box(b"lines_printed", &num_buf[..len]);
    } else if intent_type == b"display.clear" {
        // Signal kernel to clear screen (kernel handles the actual clear)
        send_intent(b"kernel.display.clear", b"");
        send_intent(b"display.clear.done", b"");
        update_glass_box(b"status", b"cleared");
    } else if intent_type == b"display.prompt" {
        // Print the AI prompt prefix
        unsafe {
            display_write(b"> ".as_ptr(), 2);
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
