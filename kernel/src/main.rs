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

use limine::request::{
    FramebufferRequest, HhdmRequest, MemoryMapRequest,
    RequestsEndMarker, RequestsStartMarker,
};

// Limine v8+ requires requests to be in a .limine_requests section
// with start/end markers so the bootloader can find them.
#[used]
#[link_section = ".limine_requests_start"]
static _REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".limine_requests"]
static FRAMEBUFFER_REQ: FramebufferRequest = FramebufferRequest::new();
#[used]
#[link_section = ".limine_requests"]
static HHDM_REQ: HhdmRequest = HhdmRequest::new();
#[used]
#[link_section = ".limine_requests"]
static MEMORY_MAP_REQ: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".limine_requests_end"]
static _REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

// Limine base revision (required by protocol — must also be in .limine_requests)
#[used]
#[link_section = ".limine_requests"]
static BASE_REVISION: limine::BaseRevision = limine::BaseRevision::new();

// Embed compiled WASM module binaries at compile time.
// These get included directly in the kernel binary. In production,
// modules would be loaded from SVFS instead.
static HELLO_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/hello-module/target/wasm32-unknown-unknown/release/hello_module.wasm"
);
static SETTINGS_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/settings-module/target/wasm32-unknown-unknown/release/squirrel_settings_module.wasm"
);
static DISPLAY_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/display-module/target/wasm32-unknown-unknown/release/squirrel_display_module.wasm"
);
static INPUT_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/input-module/target/wasm32-unknown-unknown/release/squirrel_input_module.wasm"
);
static STORAGE_MODULE_WASM: &[u8] = include_bytes!(
    "../../modules/storage-module/target/wasm32-unknown-unknown/release/squirrel_storage_module.wasm"
);

#[inline(never)]
fn serial_print(msg: &[u8]) {
    for &b in msg {
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") 0x3F8u16,
                in("al") b,
                options(nostack, nomem)
            );
        }
    }
}

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

    // 6. SVFS — content-addressed semantic storage (RAM-backed for MVP)
    {
        // Create a 2 MB RAM disk (4096 blocks × 512 bytes).
        // This is non-persistent (lost on reboot) but lets us test
        // the full SVFS stack: store, retrieve, tag queries.
        let ram_disk = alloc::boxed::Box::new(svfs::RamBlk::new(4096));
        svfs::init(ram_disk);
        println!("[OK] SVFS initialized ({} objects)", svfs::SVFS.get().unwrap().object_count());

        // Self-test: store an object, retrieve it, query by tag
        let svfs_inst = svfs::SVFS.get().unwrap();

        let hash = svfs_inst
            .store(
                b"Hello, SVFS!",
                svfs::ObjectType::Data,
                Some("test-object"),
                &["test", "hello"],
            )
            .expect("SVFS store failed");

        let retrieved = svfs_inst.retrieve(&hash).expect("SVFS retrieve failed");
        assert_eq!(retrieved, b"Hello, SVFS!");
        println!("[OK] SVFS self-test: store+retrieve verified");

        let found = svfs_inst.find_by_tag("test");
        assert!(!found.is_empty(), "SVFS tag query failed");
        println!(
            "[OK] SVFS self-test: tag query found {} object(s)",
            found.len()
        );
    }

    // 7. OS Settings — persistent configuration backed by SVFS.
    // Must be initialized after SVFS (settings are stored there) and before
    // the inference engine (which reads settings on every request).
    // On first boot: writes defaults to SVFS.
    // On subsequent boots: loads saved settings from SVFS.
    settings::set_log_fn(|msg| {
        println!("{}", msg);
    });
    settings::init();

    // 8. APIC — disable legacy PIC, enable Local APIC, start 100 Hz timer
    crate::interrupts::apic::init();
    println!("[OK] APIC + timer (100 Hz)");

    // 8a. I/O APIC — routes external IRQs (keyboard, PCI) to Local APIC vectors
    crate::interrupts::ioapic::init();

    // 8b. PS/2 keyboard driver
    crate::drivers::keyboard::init();
    println!("[OK] Keyboard");

    // 8c. Enable interrupts — needed before network init so the timer
    //     drives DHCP timeouts and smoltcp polling.
    x86_64::instructions::interrupts::enable();
    println!("[OK] Interrupts enabled");

    // 9. Network stack — auto-detects NIC (virtio-net, Intel e1000, Realtek RTL8139)
    network::set_log_fn(|msg| {
        println!("{}", msg);
    });
    let hhdm_off = crate::memory::HHDM_OFFSET.load(core::sync::atomic::Ordering::Relaxed);
    match network::init(|msg| { println!("{}", msg); }, hhdm_off) {
        Ok(()) => println!("[OK] Network stack ready"),
        Err(e) => println!("[WARN] Network: {} — continuing without network", e),
    }

    // 10. WASM runtime — set up the log bridge so WASM modules can print
    wasm_runtime::set_log_fn(|msg| {
        println!("{}", msg);
    });
    println!("[OK] WASM runtime initialized");

    // 11. Glass Box — set up the log bridge so the Glass Box agent can print
    //     the state overlay to the framebuffer. Same pattern as WASM runtime.
    glass_box::set_log_fn(|msg| {
        println!("{}", msg);
    });
    println!("[OK] Glass Box initialized");

    // 12. Inference Engine — AI text generation (cloud API for MVP)
    inference_engine::set_log_fn(|msg| {
        println!("{}", msg);
    });
    println!("[OK] Inference engine initialized");

    // 13. SART — register system agents + WASM modules
    {
        use sart::Sart;
        static SART: spin::Mutex<Sart> = spin::Mutex::new(Sart::new());

        let tick = crate::timer::ticks();
        let mut sart = SART.lock();

        // Register the Glass Box agent first — it subscribes to glass-box
        // intents so it can receive state updates from all other agents.
        sart.register(
            alloc::boxed::Box::new(glass_box::GlassBoxAgent::new(false)),
            &["glass-box.update", "glass-box.module.stopped"],
            tick,
        );
        println!("[OK] Glass Box agent registered");

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

        // Register NetworkAgent — handles "network.http.post" intents.
        // Only register if the network stack initialized successfully.
        if network::is_ready() {
            sart.register(
                alloc::boxed::Box::new(network::NetworkAgent::new()),
                &["network.http.post"],
                tick,
            );
            println!("[OK] Network agent registered");
        }

        // Register InferenceRouter — handles "inference.generate" intents.
        // This is the AI's thinking engine. It routes inference requests to
        // the right backend (local stub or cloud API) based on current settings.
        sart.register(
            alloc::boxed::Box::new(inference_engine::InferenceRouter::new()),
            &["inference.generate"],
            tick,
        );
        println!("[OK] Inference engine agent registered");

        // Register SettingsHandler — applies "settings.set" intents from the
        // WASM settings-module and "input.secret.response" (API key entry).
        sart.register(
            alloc::boxed::Box::new(agents::settings_handler::SettingsHandler::new()),
            &["settings.set", "input.secret.response"],
            tick,
        );
        println!("[OK] Settings handler agent registered");

        // Load the hello-module WASM binary and register it as a SART agent.
        // This is where the Capability Fabric comes alive: a WASM module
        // becomes a first-class agent, scheduled alongside native Rust agents.
        match wasm_runtime::WasmModule::load("hello-module", HELLO_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    sart.register(alloc::boxed::Box::new(agent), &[], tick);
                    println!("[OK] WASM: hello-module loaded ({} bytes)", HELLO_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] hello-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] hello-module load failed: {:?}", e),
        }

        // Load the settings-module WASM binary — the settings UI.
        // Subscribes to "settings.open" (triggered when user types "settings")
        // and "input.char" (keyboard input while settings screen is visible).
        match wasm_runtime::WasmModule::load("settings-module", SETTINGS_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    sart.register(
                        alloc::boxed::Box::new(agent),
                        &["settings.open", "input.char"],
                        tick,
                    );
                    println!("[OK] WASM: settings-module loaded ({} bytes)", SETTINGS_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] settings-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] settings-module load failed: {:?}", e),
        }

        // =====================================================================
        // PHASE 12: PRIMARY AI AGENT + CORE MODULES
        // =====================================================================

        // Keyboard Bridge — pumps characters from the keyboard ISR buffer
        // into "input.char" intents on the Intent Bus. This is the safe
        // bridge between interrupt context and the cooperative agent world.
        sart.register(
            alloc::boxed::Box::new(agents::keyboard_bridge::KeyboardBridgeAgent::new()),
            &[],
            tick,
        );
        println!("[OK] Keyboard bridge agent registered");

        // Display module — the ONLY path to the framebuffer.
        // All text output flows through this WASM module as "display.print" intents.
        match wasm_runtime::WasmModule::load("display-module", DISPLAY_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    sart.register(
                        alloc::boxed::Box::new(agent),
                        &["display.print", "display.clear", "display.prompt"],
                        tick,
                    );
                    println!("[OK] WASM: display-module loaded ({} bytes)", DISPLAY_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] display-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] display-module load failed: {:?}", e),
        }

        // Input module — keyboard character assembly and line editing.
        // Receives "input.char" from the keyboard bridge, emits "input.line"
        // when the user presses Enter.
        match wasm_runtime::WasmModule::load("input-module", INPUT_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    sart.register(
                        alloc::boxed::Box::new(agent),
                        &["input.char", "input.request_secret", "input.request_line"],
                        tick,
                    );
                    println!("[OK] WASM: input-module loaded ({} bytes)", INPUT_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] input-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] input-module load failed: {:?}", e),
        }

        // Storage module — SVFS proxy for WASM modules.
        // Receives "storage.store"/"storage.retrieve", forwards to kernel SVFS.
        match wasm_runtime::WasmModule::load("storage-module", STORAGE_MODULE_WASM, &[]) {
            Ok(module) => match module.instantiate() {
                Ok(agent) => {
                    sart.register(
                        alloc::boxed::Box::new(agent),
                        &["storage.store", "storage.retrieve",
                          "kernel.storage.store.response", "kernel.storage.retrieve.response"],
                        tick,
                    );
                    println!("[OK] WASM: storage-module loaded ({} bytes)", STORAGE_MODULE_WASM.len());
                }
                Err(e) => println!("[WARN] storage-module instantiation failed: {:?}", e),
            },
            Err(e) => println!("[WARN] storage-module load failed: {:?}", e),
        }

        // Primary AI Agent — Squirrel's brain. Highest priority.
        // Receives user input, pattern-matches or routes to inference,
        // displays responses. This is the top-level intelligence.
        sart.register(
            alloc::boxed::Box::new(primary_agent::PrimaryAiAgent::new()),
            &["input.line", "inference.generate.response", "system.status",
              "settings.closed", "display.clear.done"],
            tick,
        );
        println!("[OK] Primary AI Agent registered");

        println!(
            "[OK] SART: {} agents registered {:?}",
            sart.agent_count(),
            sart.agent_names()
        );
        drop(sart);

        // Glass Box self-test: send a test update through the Intent Bus.
        // This verifies the full pipeline: intent creation → bus routing →
        // GlassBoxAgent receives → GlassBoxStore updated → display renders.
        {
            use intent_bus::Intent;
            let conn = intent_bus::INTENT_BUS.connect("kernel-test", &[]);
            let intent = Intent::request(
                "glass-box.update",
                "kernel-test",
                &glass_box::GlassBoxUpdate {
                    module: alloc::string::String::from("kernel"),
                    key: alloc::string::String::from("status"),
                    value: alloc::string::String::from("booting"),
                },
            );
            conn.send(intent);
            println!("[OK] Glass Box self-test: update sent");
        }

        // 14. SART main loop
        println!("[OK] SART running");

        // Kernel idle loop — SART is driven by the main loop, woken by HLT
        // on each timer interrupt (100 Hz). This avoids running agents inside
        // the ISR context where heap allocation and mutex locking are unsafe.
        //
        // We also update the Intent Bus time each tick so that Glass Box
        // timestamps are accurate (intent_bus::bus::current_ms() is used by
        // the Glass Box store and display renderer).
        loop {
            let tick = crate::timer::ticks();
            intent_bus::bus::set_current_ms(tick * 10); // 10ms per tick
            SART.lock().tick(tick);
            x86_64::instructions::hlt();
        }
    }
}

/// Export kernel_milliseconds for the network and inference-engine crates.
///
/// The network stack (smoltcp, HTTP) and inference engine (latency tracking)
/// need to know the current time. They call
/// `extern "Rust" { fn kernel_milliseconds() -> u64; }` which links here.
#[no_mangle]
pub extern "Rust" fn kernel_milliseconds() -> u64 {
    crate::timer::milliseconds()
}

/// Export HHDM offset for the network crate's DMA address translation.
#[no_mangle]
pub extern "Rust" fn kernel_hhdm_offset() -> u64 {
    crate::memory::HHDM_OFFSET.load(core::sync::atomic::Ordering::Relaxed)
}

/// Translate a virtual address to physical using the kernel's page table.
///
/// Used by the virtio-net driver for DMA buffer addresses. Returns 0 on failure.
#[no_mangle]
pub extern "Rust" fn kernel_virt_to_phys(virt: u64) -> u64 {
    use x86_64::VirtAddr;
    let vmm = match crate::memory::VMM.get() {
        Some(v) => v,
        None => return 0,
    };
    vmm.lock()
        .translate(VirtAddr::new(virt))
        .map(|p| p.as_u64())
        .unwrap_or(0)
}

/// Allocate DMA-safe physically contiguous memory.
/// Returns (virtual_ptr, physical_addr), or (0, 0) on failure.
#[no_mangle]
pub extern "Rust" fn kernel_dma_alloc(size: usize, align: usize) -> (u64, u64) {
    crate::memory::dma::dma_alloc(size, align).unwrap_or((0, 0))
}

/// Convert a DMA virtual address to physical. Returns 0 if not a DMA address.
#[no_mangle]
pub extern "Rust" fn kernel_dma_virt_to_phys(virt: u64) -> u64 {
    crate::memory::dma::dma_virt_to_phys(virt)
}

/// Write raw text to the framebuffer without prefix or newline.
/// Used by the display-module WASM for character echo.
#[no_mangle]
pub extern "Rust" fn kernel_display_write(s: &str) {
    crate::display::write_str_raw(s);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("\n[KERNEL PANIC]\n{}", info);
    loop {
        x86_64::instructions::hlt();
    }
}
