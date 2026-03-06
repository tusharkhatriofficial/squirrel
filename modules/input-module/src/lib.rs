//! Input module — manages keyboard input, line editing, and echo to display.
//!
//! This module sits between the raw keyboard driver and the rest of the OS.
//! It receives individual keystrokes as "input.char" intents, builds them
//! into complete lines (with backspace support), and emits "input.line"
//! intents when the user presses Enter.
//!
//! It also handles "secret mode" for password/API-key entry — characters
//! are echoed as '*' instead of their actual value.
//!
//! Subscribes to:
//!   "input.char"           — single character from keyboard driver
//!   "input.request_secret" — switch to secret mode (masked echo)
//!   "input.request_line"   — switch back to normal mode
//!
//! Sends:
//!   "input.line"            — complete line of user input
//!   "input.secret.response" — complete line entered in secret mode
//!   "input.ready"           — emitted on init to signal readiness
//!   "display.print"         — echo characters back to the screen

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

const MAX_LINE: usize = 512;

/// Line buffer — accumulates characters until Enter is pressed.
static mut LINE_BUF: [u8; MAX_LINE] = [0u8; MAX_LINE];
/// Current position in the line buffer.
static mut LINE_LEN: usize = 0;
/// When true, characters are echoed as '*' (for API keys, passwords).
static mut SECRET_MODE: bool = false;
/// Total characters typed since boot (Glass Box metric).
static mut CHARS_TYPED: u32 = 0;

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
    send_intent(b"input.ready", b"");
    update_glass_box(b"status", b"ready");
}

/// Called every scheduler tick (100 Hz).
///
/// Checks for incoming intents. When "input.char" arrives, the character
/// is added to the line buffer and echoed to the display. When Enter is
/// pressed, the complete line is sent as "input.line".
#[no_mangle]
pub extern "C" fn poll() {
    let mut buf = [0u8; 512];
    let n = unsafe { intent_recv(buf.as_mut_ptr(), buf.len() as i32) };

    if n <= 0 {
        return;
    }

    // Parse intent: [2-byte type_len][type_bytes][remaining payload]
    let n = n as usize;
    if n < 2 {
        return;
    }

    let type_len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    if 2 + type_len > n {
        return;
    }

    let intent_type = &buf[2..2 + type_len];
    let payload = &buf[2 + type_len..n];

    if intent_type == b"input.char" {
        if !payload.is_empty() {
            handle_char(payload[0]);
        }
    } else if intent_type == b"input.request_secret" {
        unsafe { SECRET_MODE = true; }
        update_glass_box(b"mode", b"secret");
    } else if intent_type == b"input.request_line" {
        unsafe { SECRET_MODE = false; }
        update_glass_box(b"mode", b"normal");
    }
}

// ---------------------------------------------------------------------------
// Character handling and line editing
// ---------------------------------------------------------------------------

/// Process a single character: append to buffer, handle Enter and Backspace.
fn handle_char(ch: u8) {
    unsafe {
        CHARS_TYPED += 1;

        match ch {
            // Enter — line complete
            b'\n' | b'\r' => {
                let line = &LINE_BUF[..LINE_LEN];
                if SECRET_MODE {
                    send_intent(b"input.secret.response", line);
                } else {
                    send_intent(b"input.line", line);
                }
                // Echo newline to display
                send_intent(b"display.print", b"\n");

                // Reset line buffer
                LINE_LEN = 0;
                SECRET_MODE = false;
                update_glass_box(b"mode", b"normal");
            }

            // Backspace — erase last character
            0x08 | 0x7F => {
                if LINE_LEN > 0 {
                    LINE_LEN -= 1;
                    // Erase from display: backspace + space + backspace
                    send_intent(b"display.print", b"\x08 \x08");
                }
            }

            // Printable ASCII — add to buffer and echo
            c if (c == b' ') || (c > b' ' && c < 0x7F) => {
                if LINE_LEN < MAX_LINE {
                    LINE_BUF[LINE_LEN] = c;
                    LINE_LEN += 1;

                    // Echo to display (masked if in secret mode)
                    let echo: [u8; 1] = if SECRET_MODE { [b'*'] } else { [c] };
                    send_intent(b"display.print", &echo);
                }
            }

            // Ignore non-printable characters
            _ => {}
        }

        // Update Glass Box with character count
        let mut num_buf = [0u8; 10];
        let len = u32_to_ascii(CHARS_TYPED, &mut num_buf);
        update_glass_box(b"chars_typed", &num_buf[..len]);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
