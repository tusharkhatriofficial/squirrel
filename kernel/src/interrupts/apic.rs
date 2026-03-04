//! Local APIC driver — replaces the legacy 8259 PIC.
//!
//! The Local APIC (Advanced Programmable Interrupt Controller) is the modern
//! interrupt delivery mechanism on x86_64. Each CPU core has its own Local APIC
//! at the well-known physical address 0xFEE0_0000. We access it via Limine's
//! Higher Half Direct Map (HHDM), which identity-maps all physical memory into
//! the upper half of virtual address space.
//!
//! This module:
//! - Disables the legacy 8259 PIC (masks all IRQs)
//! - Enables the Local APIC via the spurious interrupt vector register
//! - Calibrates and starts a periodic timer at 100 Hz (vector 0x20)
//! - Provides EOI (End Of Interrupt) signaling

use crate::println;
use core::sync::atomic::Ordering;
use spin::Mutex;

/// Physical base address of the Local APIC (standard on all x86_64 CPUs)
const APIC_PHYS_BASE: u64 = 0xFEE0_0000;

// APIC register offsets (each register is at a 16-byte aligned offset,
// but we address them as u32 indices into 4-byte slots)
const REG_EOI: usize = 0xB0 / 4;
const REG_SPURIOUS: usize = 0xF0 / 4;
const REG_TIMER_LVT: usize = 0x320 / 4;
const REG_TIMER_INITIAL: usize = 0x380 / 4;
const REG_TIMER_CURRENT: usize = 0x390 / 4;
const REG_TIMER_DIVIDE: usize = 0x3E0 / 4;

pub static APIC: Mutex<LocalApic> = Mutex::new(LocalApic { base: 0 });

pub struct LocalApic {
    /// Virtual address of the APIC MMIO region (set during init)
    base: usize,
}

impl LocalApic {
    /// Read a 32-bit APIC register via MMIO.
    unsafe fn read(&self, reg: usize) -> u32 {
        let ptr = (self.base + reg * 4) as *const u32;
        core::ptr::read_volatile(ptr)
    }

    /// Write a 32-bit APIC register via MMIO.
    unsafe fn write(&self, reg: usize, val: u32) {
        let ptr = (self.base + reg * 4) as *mut u32;
        core::ptr::write_volatile(ptr, val);
    }

    /// Signal End Of Interrupt to the APIC. Must be called at the end of
    /// every hardware interrupt handler (except spurious).
    pub fn send_eoi(&self) {
        unsafe {
            self.write(REG_EOI, 0);
        }
    }

    /// Enable the Local APIC by setting bit 8 (APIC Software Enable) in the
    /// Spurious Interrupt Vector Register. The spurious vector is set to 0xFF.
    fn enable(&self) {
        unsafe {
            self.write(REG_SPURIOUS, 0x1FF);
        }
    }

    /// Calibrate and start the APIC timer in periodic mode at the given frequency.
    ///
    /// Calibration uses the legacy PIT (Programmable Interval Timer) channel 2
    /// as a reference: we let the APIC timer free-run while waiting ~10ms on
    /// the PIT, then compute how many APIC ticks correspond to one timer period.
    fn init_timer(&self, hz: u32) {
        use x86_64::instructions::port::Port;

        unsafe {
            // Set APIC timer divide configuration to divide-by-16
            self.write(REG_TIMER_DIVIDE, 0x3);

            // Start the APIC timer counting down from max to calibrate
            self.write(REG_TIMER_INITIAL, u32::MAX);

            // --- PIT channel 2 calibration (~10ms) ---
            // PIT oscillates at 1,193,182 Hz. 11,932 ticks ≈ 10ms.
            let mut pit_cmd: Port<u8> = Port::new(0x43);
            let mut pit_ch2: Port<u8> = Port::new(0x42);
            let mut pit_gate: Port<u8> = Port::new(0x61);

            // Enable PIT channel 2 gate, disable speaker
            let gate = (pit_gate.read() & 0xFD) | 0x01;
            pit_gate.write(gate);

            // Configure channel 2: mode 0 (interrupt on terminal count),
            // binary counting, lo/hi byte access
            pit_cmd.write(0xB0);
            // Load count = 0x2E9C (11,932 decimal) ≈ 10ms
            pit_ch2.write(0x9C); // low byte
            pit_ch2.write(0x2E); // high byte

            // Wait for PIT to count down — poll bit 5 of port 0x61
            // (OUT2 pin goes high when channel 2 reaches zero)
            while pit_gate.read() & 0x20 == 0 {}

            // Read how many APIC ticks elapsed during the ~10ms PIT window
            let elapsed = u32::MAX - self.read(REG_TIMER_CURRENT);

            // elapsed ticks in ~10ms → ticks per second = elapsed * 100
            // For a periodic timer at `hz` Hz: initial_count = ticks_per_second / hz
            // Simplified: initial_count = elapsed * 100 / hz = elapsed / (hz / 100)
            // Since hz=100: initial_count = elapsed (one 10ms period = one tick at 100Hz)
            let count = (elapsed as u64 * 100 / hz as u64) as u32;

            // Configure periodic timer on vector 0x20
            // Bit 17 = periodic mode, bits 0-7 = interrupt vector
            self.write(REG_TIMER_LVT, 0x0002_0020);
            self.write(REG_TIMER_INITIAL, count);
        }
    }
}

/// Initialize the Local APIC: disable legacy PIC, enable APIC, start 100 Hz timer.
///
/// Must be called after memory::init() (needs HHDM_OFFSET) and before
/// enabling interrupts with `sti`.
pub fn init() {
    // Compute virtual address of APIC MMIO via Limine's HHDM
    let hhdm = crate::memory::HHDM_OFFSET.load(Ordering::Relaxed);
    let apic_virt = hhdm + APIC_PHYS_BASE;

    // Disable the legacy 8259 PIC by masking all IRQs on both chips.
    // This prevents spurious legacy interrupts that could cause conflicts
    // with the APIC-based interrupt routing.
    unsafe {
        use x86_64::instructions::port::Port;
        Port::<u8>::new(0xA1).write(0xFF); // slave PIC: mask all
        Port::<u8>::new(0x21).write(0xFF); // master PIC: mask all
    }

    let mut apic = APIC.lock();
    apic.base = apic_virt as usize;
    apic.enable();
    apic.init_timer(100); // 100 Hz periodic timer

    println!("[HW] APIC: initialized, timer at 100 Hz");
}
