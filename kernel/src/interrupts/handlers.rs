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
