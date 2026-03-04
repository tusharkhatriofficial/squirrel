//! PS/2 keyboard driver — translates scancodes into characters.
//!
//! The PS/2 keyboard controller sends scancodes via IRQ 1 (APIC vector 0x21).
//! The interrupt handler reads port 0x60 and calls handle_scancode(), which
//! uses the pc-keyboard crate to decode ScancodeSet1 into Unicode characters.
//!
//! Characters are buffered in a VecDeque ring buffer. The main loop polls
//! via try_read_char(). Access is synchronized with without_interrupts()
//! to prevent deadlocks between the IRQ handler and main-loop reader.

use crate::println;
use alloc::collections::VecDeque;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::{Mutex, Once};

/// Decoded character buffer — producer is the IRQ handler, consumer is the kernel.
/// Protected by Mutex; callers use without_interrupts() to avoid deadlock.
static INPUT_BUFFER: Once<Mutex<VecDeque<char>>> = Once::new();

/// PC keyboard decoder — only accessed from the IRQ handler (interrupts disabled),
/// so the Mutex is never contended, but it gives us interior mutability for statics.
static KEYBOARD: Once<Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>>> = Once::new();

/// Initialize the keyboard driver. Must be called after the heap is available
/// (needs alloc for VecDeque).
pub fn init() {
    INPUT_BUFFER.call_once(|| Mutex::new(VecDeque::with_capacity(256)));
    KEYBOARD.call_once(|| {
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore,
        ))
    });
    println!("[HW] Keyboard: PS/2 initialized");
}

/// Called from the keyboard IRQ handler (vector 0x21).
/// Decodes the raw scancode into a character and pushes it into the buffer.
/// Runs with interrupts disabled (inside ISR), so Mutex access is safe.
pub fn handle_scancode(scancode: u8) {
    let kb = match KEYBOARD.get() {
        Some(kb) => kb,
        None => return, // Driver not initialized yet
    };

    let mut kb = kb.lock();
    if let Ok(Some(evt)) = kb.add_byte(scancode) {
        if let Some(key) = kb.process_keyevent(evt) {
            match key {
                DecodedKey::Unicode(c) => {
                    if let Some(buf) = INPUT_BUFFER.get() {
                        let mut q = buf.lock();
                        if q.len() < 256 {
                            q.push_back(c);
                        }
                        // Drop characters if buffer is full (keyboard ahead of consumer)
                    }
                }
                DecodedKey::RawKey(_) => {
                    // Ignore non-Unicode keys (Shift, Ctrl, etc.) for now
                }
            }
        }
    }
}

/// Pop one character from the input buffer (non-blocking).
/// Returns None if the buffer is empty.
/// Uses without_interrupts() to prevent deadlock with the IRQ handler.
pub fn try_read_char() -> Option<char> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        INPUT_BUFFER.get()?.lock().pop_front()
    })
}
