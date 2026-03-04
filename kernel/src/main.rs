#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod display;
mod gdt;
mod interrupts;
mod memory;

use limine::request::{FramebufferRequest, HhdmRequest, MemoryMapRequest};

// Tell Limine what we need
#[used]
static FRAMEBUFFER_REQ: FramebufferRequest = FramebufferRequest::new();
#[used]
static HHDM_REQ: HhdmRequest = HhdmRequest::new();
#[used]
static MEMORY_MAP_REQ: MemoryMapRequest = MemoryMapRequest::new();

// Limine base revision (required by protocol)
#[used]
static BASE_REVISION: limine::BaseRevision = limine::BaseRevision::new();

#[no_mangle]
pub extern "C" fn _start() -> ! {
    assert!(
        BASE_REVISION.is_supported(),
        "Limine base revision not supported"
    );

    // 1. Init early display (must be first so we can print errors)
    let fb_resp = FRAMEBUFFER_REQ
        .get_response()
        .expect("no framebuffer response");
    let fb = fb_resp.framebuffers().next().expect("no framebuffer");
    crate::display::init(&fb);

    println!("Squirrel AIOS v0.1.0");
    println!("Kernel loaded. Initializing...");

    // 2. GDT
    crate::gdt::init();
    println!("[OK] GDT");

    // 3. IDT (exceptions only for now; hardware IRQs added in Phase 03)
    crate::interrupts::init_idt();
    println!("[OK] IDT");

    // 4. Pass memory map to memory manager (Phase 02 fills this in)
    let mmap = MEMORY_MAP_REQ
        .get_response()
        .expect("no memory map");
    let hhdm = HHDM_REQ.get_response().expect("no HHDM response");
    crate::memory::init(mmap, hhdm.offset());
    println!("[OK] Memory (stub)");

    println!("Kernel core initialized. Halting until SART is ready.");
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("\n[KERNEL PANIC]\n{}", info);
    loop {
        x86_64::instructions::hlt();
    }
}
