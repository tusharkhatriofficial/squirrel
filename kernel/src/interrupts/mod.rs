use x86_64::structures::idt::InterruptDescriptorTable;
use spin::Once;

pub mod apic;
pub mod handlers;

static IDT: Once<InterruptDescriptorTable> = Once::new();

pub fn init_idt() {
    let idt = IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();

        // CPU exceptions
        idt.breakpoint.set_handler_fn(handlers::breakpoint);
        idt.invalid_opcode.set_handler_fn(handlers::invalid_opcode);
        idt.general_protection_fault
            .set_handler_fn(handlers::general_protection_fault);
        idt.page_fault.set_handler_fn(handlers::page_fault);
        unsafe {
            idt.double_fault
                .set_handler_fn(handlers::double_fault)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Hardware IRQ vectors (APIC-routed)
        idt[0x20].set_handler_fn(handlers::timer);      // APIC timer (100 Hz)
        idt[0x21].set_handler_fn(handlers::keyboard);   // PS/2 keyboard
        idt[0x22].set_handler_fn(handlers::network_rx); // virtio-net (Phase 09)
        idt[0xFF].set_handler_fn(handlers::spurious);   // APIC spurious

        idt
    });
    idt.load();
}
