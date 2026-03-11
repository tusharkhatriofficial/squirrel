//! I/O APIC driver — routes external hardware IRQs to Local APIC vectors.
//!
//! The I/O APIC sits at physical address 0xFEC0_0000 and translates external
//! interrupt sources (keyboard IRQ1, network IRQ, etc.) into messages delivered
//! to the Local APIC on the BSP (bootstrap processor).
//!
//! Each I/O APIC pin has a 64-bit redirection entry that specifies:
//! - The interrupt vector (0x20-0xFE)
//! - Delivery mode (fixed, lowest priority, etc.)
//! - Destination APIC ID
//! - Trigger mode (edge/level) and polarity

use core::sync::atomic::Ordering;

/// Standard physical base address of the I/O APIC
const IOAPIC_PHYS_BASE: u64 = 0xFEC0_0000;

/// I/O APIC register offsets (accessed indirectly via IOREGSEL/IOWIN)
const IOREGSEL: usize = 0x00; // Register select (write index here)
const IOWIN: usize = 0x10;    // Read/write the selected register

/// I/O APIC registers
const REG_ID: u32 = 0x00;
const REG_VER: u32 = 0x01;
const REG_REDTBL_BASE: u32 = 0x10; // Redirection table starts here (2 regs per entry)

/// Virtual base address of the I/O APIC MMIO (set during init)
static mut IOAPIC_BASE: usize = 0;

/// Read an I/O APIC register via the indirect register access mechanism.
unsafe fn read(reg: u32) -> u32 {
    let base = IOAPIC_BASE;
    core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg);
    core::ptr::read_volatile((base + IOWIN) as *const u32)
}

/// Write an I/O APIC register via the indirect register access mechanism.
unsafe fn write(reg: u32, val: u32) {
    let base = IOAPIC_BASE;
    core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg);
    core::ptr::write_volatile((base + IOWIN) as *mut u32, val);
}

/// Write a 64-bit redirection entry for the given IRQ pin.
///
/// The entry is split across two 32-bit registers:
/// - Low 32 bits: vector, delivery mode, trigger, polarity, mask
/// - High 32 bits: destination APIC ID (bits 56-63 of the entry)
unsafe fn write_redirection(irq: u8, entry: u64) {
    let reg = REG_REDTBL_BASE + (irq as u32) * 2;
    write(reg, entry as u32);           // low 32 bits
    write(reg + 1, (entry >> 32) as u32); // high 32 bits
}

/// Build a redirection entry that routes an IRQ to `vector` on APIC ID 0 (BSP).
///
/// Fixed delivery, physical destination, edge-triggered, active-high, unmasked.
fn redirection_entry(vector: u8) -> u64 {
    // Bits 0-7:   vector
    // Bits 8-10:  delivery mode (000 = Fixed)
    // Bit 11:     destination mode (0 = Physical)
    // Bit 13:     polarity (0 = Active High)
    // Bit 15:     trigger mode (0 = Edge)
    // Bit 16:     mask (0 = Unmasked)
    // Bits 56-63: destination APIC ID (0 = BSP)
    vector as u64
}

/// Initialize the I/O APIC: map its MMIO page and set up IRQ routing.
///
/// Routes:
/// - IRQ 1 (PS/2 keyboard) → vector 0x21
/// - IRQ 11 (PCI/virtio-net, typical) → vector 0x22
pub fn init() {
    use x86_64::structures::paging::{PageTableFlags, PhysFrame};
    use x86_64::{PhysAddr, VirtAddr};

    let hhdm = crate::memory::HHDM_OFFSET.load(Ordering::Relaxed);
    let ioapic_virt = hhdm + IOAPIC_PHYS_BASE;

    // Map the I/O APIC MMIO page
    {
        let vmm = crate::memory::VMM.get().expect("VMM not initialized");
        let pmm = crate::memory::PMM.get().expect("PMM not initialized");
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_CACHE;
        let phys_frame = PhysFrame::containing_address(PhysAddr::new(IOAPIC_PHYS_BASE));
        let _ = vmm.lock().map_page(VirtAddr::new(ioapic_virt), phys_frame, flags, pmm);
    }

    unsafe {
        IOAPIC_BASE = ioapic_virt as usize;

        // Read I/O APIC version to get max redirection entries
        let ver = read(REG_VER);
        let max_entries = ((ver >> 16) & 0xFF) + 1;

        // Mask all IRQ pins first (set bit 16 = masked)
        for i in 0..max_entries as u8 {
            write_redirection(i, 1 << 16); // masked
        }

        // Route IRQ 1 (PS/2 keyboard) → vector 0x21
        write_redirection(1, redirection_entry(0x21));

        // Route IRQ 11 → vector 0x22 (common PCI interrupt for virtio-net)
        write_redirection(11, redirection_entry(0x22));
    }

    crate::println!("[HW] I/O APIC: initialized, IRQ1→0x21, IRQ11→0x22");
}
