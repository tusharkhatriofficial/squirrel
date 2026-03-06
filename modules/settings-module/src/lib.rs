//! Settings Module — WASM capability module for the Squirrel settings UI.
//!
//! When the user types "settings" at the Squirrel prompt, this module takes
//! over the display and shows an interactive settings screen. The user can:
//!
//!   - See which AI backend is currently active
//!   - Switch between local, OpenAI, Anthropic, Gemini, or custom
//!   - Enter API keys (masked with * characters)
//!
//! This module runs as WebAssembly inside the WASM runtime. It communicates
//! with the kernel through intents:
//!
//!   Sends:
//!     "settings.set"       — change a setting value
//!     "display.print"      — render text to the framebuffer
//!     "input.request_secret" — ask for masked keyboard input
//!     "settings.closed"    — notify that the settings screen is dismissed
//!
//!   Receives:
//!     "settings.open"      — SART tells us to show the settings screen
//!     "input.char"         — keyboard input from the user

#![no_std]

// Import the host functions provided by the kernel's WASM runtime.
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
    fn time_ns() -> i64;
    fn log(msg_ptr: *const u8, msg_len: i32);
}

/// Whether the settings screen is currently active (visible to the user).
static mut ACTIVE: bool = false;

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn log_msg(msg: &[u8]) {
    unsafe {
        log(msg.as_ptr(), msg.len() as i32);
    }
}

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

// ---------------------------------------------------------------------------
// Module lifecycle (called by SART)
// ---------------------------------------------------------------------------

/// Called once when the module is loaded by SART.
#[no_mangle]
pub extern "C" fn init() {
    log_msg(b"Settings module loaded");
    update_glass_box(b"status", b"ready");
}

/// Called every scheduler tick (100 Hz) by SART.
///
/// Checks for incoming intents. When "settings.open" arrives, it shows
/// the settings screen and starts processing keyboard input.
#[no_mangle]
pub extern "C" fn poll() {
    // Check for incoming intents
    let mut buf = [0u8; 512];
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

    // "settings.open" — show the settings screen
    if intent_type == b"settings.open" {
        unsafe { ACTIVE = true; }
        render_settings_screen();
        return;
    }

    // Only process input when the settings screen is active
    if !unsafe { ACTIVE } {
        return;
    }

    // "input.char" — handle keyboard input
    if intent_type == b"input.char" {
        if payload_len > 0 && payload_start < n {
            handle_input(buf[payload_start]);
        }
    }
}

// ---------------------------------------------------------------------------
// Settings screen rendering
// ---------------------------------------------------------------------------

/// Render the main settings screen.
///
/// Shows numbered options for each AI backend. The user types a number
/// to select, or presses Enter to cancel and go back.
fn render_settings_screen() {
    send_intent(b"display.print", b"\n");
    send_intent(b"display.print", b"+----------------------------------------------------+\n");
    send_intent(b"display.print", b"|  Squirrel AIOS - Settings                          |\n");
    send_intent(b"display.print", b"+----------------------------------------------------+\n");
    send_intent(b"display.print", b"|  AI Inference Backend:                              |\n");
    send_intent(b"display.print", b"|                                                    |\n");
    send_intent(b"display.print", b"|  [1] local       - on-device GGUF model             |\n");
    send_intent(b"display.print", b"|  [2] openai      - GPT-4o                           |\n");
    send_intent(b"display.print", b"|  [3] anthropic   - Claude Sonnet 4.6                |\n");
    send_intent(b"display.print", b"|  [4] gemini      - Gemini 2.0 Flash                 |\n");
    send_intent(b"display.print", b"|  [5] custom      - your own endpoint                |\n");
    send_intent(b"display.print", b"|                                                    |\n");
    send_intent(b"display.print", b"|  Type a number to switch, or Enter to cancel.       |\n");
    send_intent(b"display.print", b"+----------------------------------------------------+\n\n");

    update_glass_box(b"status", b"settings-screen-visible");
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

/// Handle a single character of keyboard input while settings is active.
fn handle_input(ch: u8) {
    match ch {
        b'1' => set_backend(b"local"),
        b'2' => set_backend_cloud(b"openai", b"gpt-4o"),
        b'3' => set_backend_cloud(b"anthropic", b"claude-sonnet-4-6"),
        b'4' => set_backend_cloud(b"gemini", b"gemini-2.0-flash"),
        b'5' => {
            send_intent(b"display.print", b"  Enter custom API base URL: ");
            send_intent(b"input.request_line", b"");
        }
        b'\n' | b'\r' => close_settings(),
        _ => {} // Ignore other keys
    }
}

/// Switch to the local backend (no API key needed).
///
/// Sends a "settings.set" intent with payload "inference.backend\0local".
/// The null byte separates the key from the value.
fn set_backend(backend: &[u8]) {
    let mut payload = [0u8; 64];
    let key = b"inference.backend\x00";
    let total = key.len() + backend.len();
    payload[..key.len()].copy_from_slice(key);
    payload[key.len()..total].copy_from_slice(backend);

    send_intent(b"settings.set", &payload[..total]);
    send_intent(b"display.print", b"\n  Backend set to: local\n");
    close_settings();
}

/// Switch to a cloud backend.
///
/// Sets both the backend name and model ID, then prompts for an API key.
/// The API key is entered through masked input (characters shown as *).
fn set_backend_cloud(backend: &[u8], model: &[u8]) {
    // Set inference.backend
    let mut payload = [0u8; 64];
    let key = b"inference.backend\x00";
    let total = key.len() + backend.len();
    payload[..key.len()].copy_from_slice(key);
    payload[key.len()..total].copy_from_slice(backend);
    send_intent(b"settings.set", &payload[..total]);

    // Set inference.model_id
    let mut payload2 = [0u8; 64];
    let key2 = b"inference.model_id\x00";
    let total2 = key2.len() + model.len();
    payload2[..key2.len()].copy_from_slice(key2);
    payload2[key2.len()..total2].copy_from_slice(model);
    send_intent(b"settings.set", &payload2[..total2]);

    // Prompt for API key with masked input
    send_intent(b"display.print", b"\n  Enter API key (hidden): ");
    send_intent(b"input.request_secret", backend);
}

/// Close the settings screen and return to normal operation.
fn close_settings() {
    unsafe { ACTIVE = false; }
    send_intent(b"display.print", b"\n  Settings saved. Returning...\n\n");
    send_intent(b"settings.closed", b"");
    update_glass_box(b"status", b"idle");
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
