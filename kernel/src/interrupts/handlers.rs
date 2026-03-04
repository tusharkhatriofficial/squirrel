use crate::println;
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

pub extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    println!("[EXCEPTION] Breakpoint @ {:#x}", frame.instruction_pointer);
}

pub extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
    panic!("INVALID OPCODE\n{:#?}", frame);
}

pub extern "x86-interrupt" fn general_protection_fault(frame: InterruptStackFrame, error: u64) {
    panic!(
        "GENERAL PROTECTION FAULT (error={:#x})\n{:#?}",
        error, frame
    );
}

pub extern "x86-interrupt" fn page_fault(
    frame: InterruptStackFrame,
    error: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    panic!(
        "PAGE FAULT\nAddress: {:?}\nError: {:?}\n{:#?}",
        Cr2::read(),
        error,
        frame
    );
}

pub extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, _error: u64) -> ! {
    panic!("DOUBLE FAULT\n{:#?}", frame);
}

// --- Hardware IRQ handlers (vectors 0x20+) ---

/// Vector 0x20: APIC Timer — fires at 100 Hz.
/// Increments the global tick counter. SART scheduler hooks in at Phase 05.
pub extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    crate::timer::TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    crate::interrupts::apic::APIC.lock().send_eoi();
}

/// Vector 0x21: PS/2 Keyboard — fires on every keypress/release.
/// Reads the scancode from port 0x60 and hands it to the keyboard driver.
pub extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    let scancode: u8 = unsafe { Port::new(0x60).read() };
    crate::drivers::keyboard::handle_scancode(scancode);
    crate::interrupts::apic::APIC.lock().send_eoi();
}

/// Vector 0x22: Network RX (virtio-net) — placeholder for Phase 09.
pub extern "x86-interrupt" fn network_rx(_frame: InterruptStackFrame) {
    crate::interrupts::apic::APIC.lock().send_eoi();
}

/// Vector 0xFF: APIC Spurious interrupt.
/// Per Intel spec, do NOT send EOI for spurious interrupts.
pub extern "x86-interrupt" fn spurious(_frame: InterruptStackFrame) {
    // Intentionally empty — no EOI
}
