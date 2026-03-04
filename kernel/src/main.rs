#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod agents;
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

// Embed the compiled hello-module WASM binary at compile time.
// This is 625 bytes of WebAssembly bytecode that gets included
// directly in the kernel binary. In Phase 07, modules will be
// loaded from SVFS instead.
static HELLO_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/hello-module/target/wasm32-unknown-unknown/release/hello_module.wasm"
);

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

    // 5. Intent Bus — the messaging backbone (static global, no init needed)
    {
        use intent_bus::INTENT_BUS;
        use intent_bus::Intent;
        use serde::{Serialize, Deserialize};

        println!("[OK] Intent Bus");

        // Self-test: send a test intent and verify it's received
        #[derive(Serialize, Deserialize, Debug)]
        struct TestMsg { value: u32 }

        let conn_a = INTENT_BUS.connect("test-sender", &[]);
        let conn_b = INTENT_BUS.connect("test-receiver", &["test.ping"]);

        let intent = Intent::request("test.ping", "test-sender", &TestMsg { value: 42 });
        conn_a.send(intent);

        if let Some(received) = conn_b.try_recv() {
            let msg: TestMsg = received.decode().unwrap();
            assert_eq!(msg.value, 42);
            println!("[OK] Intent Bus self-test passed (value={})", msg.value);
        } else {
            panic!("Intent Bus self-test failed: no message received");
        }
    }

    // 6. APIC — disable legacy PIC, enable Local APIC, start 100 Hz timer
    crate::interrupts::apic::init();
    println!("[OK] APIC + timer (100 Hz)");

    // 7. PS/2 keyboard driver
    crate::drivers::keyboard::init();
    println!("[OK] Keyboard");

    // 8. Scan for virtio-net device (informational — real driver in Phase 09)
    if crate::drivers::network::virtio_net::VirtioNetDevice::find().is_none() {
        println!("[HW] virtio-net: not found (normal without QEMU -device flag)");
    }

    // 9. WASM runtime — set up the log bridge so WASM modules can print
    wasm_runtime::set_log_fn(|msg| {
        println!("{}", msg);
    });
    println!("[OK] WASM runtime initialized");

    // 10. SART — register test agents + WASM modules
    {
        use sart::Sart;
        static SART: spin::Mutex<Sart> = spin::Mutex::new(Sart::new());

        let tick = crate::timer::ticks();
        let mut sart = SART.lock();
        sart.register(
            alloc::boxed::Box::new(agents::heartbeat::HeartbeatAgent::new()),
            &[],
            tick,
        );
        sart.register(
            alloc::boxed::Box::new(agents::echo::EchoAgent),
            &["system.heartbeat"],
            tick,
        );

        // Load the hello-module WASM binary and register it as a SART agent.
        // This is where the Capability Fabric comes alive: a WASM module
        // becomes a first-class agent, scheduled alongside native Rust agents.
        match wasm_runtime::WasmModule::load("hello-module", HELLO_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    // hello-module has no subscriptions — it only sends intents.
                    sart.register(alloc::boxed::Box::new(agent), &[], tick);
                    println!("[OK] WASM: hello-module loaded ({} bytes)", HELLO_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] hello-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] hello-module load failed: {:?}", e),
        }

        println!(
            "[OK] SART: {} agents registered {:?}",
            sart.agent_count(),
            sart.agent_names()
        );
        drop(sart);

        // 11. Enable interrupts — APIC and IDT must be ready before this
        x86_64::instructions::interrupts::enable();
        println!("[OK] Interrupts enabled — SART running");

        // Kernel idle loop — SART is driven by the main loop, woken by HLT
        // on each timer interrupt (100 Hz). This avoids running agents inside
        // the ISR context where heap allocation and mutex locking are unsafe.
        loop {
            SART.lock().tick(crate::timer::ticks());
            x86_64::instructions::hlt();
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("\n[KERNEL PANIC]\n{}", info);
    loop {
        x86_64::instructions::hlt();
    }
}
