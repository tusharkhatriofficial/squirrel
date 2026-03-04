#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod display;
mod drivers;
mod gdt;
mod interrupts;
mod memory;
mod timer;

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

    // 3. IDT (exceptions + hardware IRQs)
    crate::interrupts::init_idt();
    println!("[OK] IDT");

    // 4. Memory manager: PMM, VMM, heap
    let mmap = MEMORY_MAP_REQ
        .get_response()
        .expect("no memory map");
    let hhdm = HHDM_REQ.get_response().expect("no HHDM response");
    crate::memory::init(mmap, hhdm.offset());
    println!("[OK] Memory");

    // Verify alloc works with Box, Vec, String
    {
        use alloc::{boxed::Box, string::String, vec};
        let b = Box::new(0xDEADBEEFu64);
        assert_eq!(*b, 0xDEADBEEF);
        let v = vec![1u32, 2, 3, 4, 5];
        assert_eq!(v.len(), 5);
        let s = String::from("Squirrel");
        assert_eq!(s.len(), 8);
        println!(
            "[OK] Heap: Box={:#x}, Vec len={}, String={}",
            *b,
            v.len(),
            s
        );
    }

    // 5. APIC — disable legacy PIC, enable Local APIC, start 100 Hz timer
    crate::interrupts::apic::init();
    println!("[OK] APIC + timer (100 Hz)");

    // 6. PS/2 keyboard driver
    crate::drivers::keyboard::init();
    println!("[OK] Keyboard");

    // 7. Scan for virtio-net device (informational — real driver in Phase 09)
    if crate::drivers::network::virtio_net::VirtioNetDevice::find().is_none() {
        println!("[HW] virtio-net: not found (normal without QEMU -device flag)");
    }

    // 8. Enable interrupts — APIC and IDT must be ready before this
    x86_64::instructions::interrupts::enable();
    println!("[OK] Interrupts enabled");

    // Keyboard echo loop — characters typed in QEMU appear on screen.
    // This is a temporary test loop; Phase 05 replaces it with the SART scheduler.
    println!("Type something (keyboard test):");
    loop {
        if let Some(c) = crate::drivers::keyboard::try_read_char() {
            print!("{}", c);
        }
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
