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

        // Hardware IRQ vectors (0x20-0x2F) — set in Phase 03
        idt
    });
    idt.load();
}
