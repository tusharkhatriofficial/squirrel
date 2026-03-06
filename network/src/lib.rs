//! Squirrel AIOS Network Stack
//!
//! Complete network stack for bare-metal x86_64: virtio-net Ethernet driver,
//! smoltcp TCP/IP, and an HTTP/1.1 client. All wrapped in a SART agent
//! that responds to Intent Bus messages.
//!
//! Layer diagram:
//!
//!   ┌─────────────────────────────────────┐
//!   │  NetworkAgent (SART agent)          │  Handles "network.http.post"
//!   ├─────────────────────────────────────┤
//!   │  HttpClient                         │  HTTP/1.1 POST + JSON
//!   ├─────────────────────────────────────┤
//!   │  NetworkStack (smoltcp)             │  DHCP, DNS, TCP sockets
//!   ├─────────────────────────────────────┤
//!   │  SmoltcpAdapter                     │  Device trait bridge
//!   ├─────────────────────────────────────┤
//!   │  VirtioNet                          │  Virtqueue-based Ethernet
//!   └─────────────────────────────────────┘
//!
//! TLS 1.3 is provided by embedded-tls (pure Rust, no C dependencies).

#![no_std]
extern crate alloc;

pub mod agent;
pub mod http;
pub mod rng;
pub mod stack;
pub mod tls;
pub mod virtio_net;

pub use agent::{NetworkAgent, NetworkRequest, NetworkResponse};
pub use stack::NetworkStack;

use spin::{Mutex, Once};

/// Global network stack instance.
///
/// Initialized by init() after DHCP completes. Wrapped in Once<Mutex<>>
/// so it's safe to access from multiple agents (the NetworkAgent polls it
/// every tick, and other agents can check network readiness).
static NETWORK_STACK: Once<Mutex<NetworkStack>> = Once::new();

/// Whether the network stack was initialized successfully.
static NET_READY: Once<()> = Once::new();

/// Initialize the network stack: find virtio-net, set up driver, run DHCP.
///
/// Called once from kernel main. If no virtio-net device is found (e.g. QEMU
/// launched without -device virtio-net-pci), returns Err and the kernel
/// continues without networking.
///
/// On success, the global NETWORK_STACK is populated and ready for use by
/// the NetworkAgent.
pub fn init(log_fn: fn(&str)) -> Result<(), &'static str> {
    // 1. Find the virtio-net PCI device
    let (io_base, pci_slot) = virtio_net::find_virtio_net()
        .ok_or("No virtio-net device found")?;
    log_fn(&alloc::format!(
        "[Network] virtio-net found at PCI 0:{}.0, I/O base 0x{:04X}",
        pci_slot, io_base
    ));

    // 2. Initialize the driver (handshake, set up virtqueues, read MAC)
    let dev = virtio_net::VirtioNet::new(io_base);
    log_fn(&alloc::format!(
        "[Network] MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        dev.mac[0], dev.mac[1], dev.mac[2],
        dev.mac[3], dev.mac[4], dev.mac[5]
    ));

    // 3. Create the TCP/IP stack
    let mut net_stack = NetworkStack::new(dev);

    // 4. Run DHCP to acquire an IP address
    log_fn("[Network] Running DHCP...");
    let ip = net_stack.run_dhcp()?;
    log_fn(&alloc::format!(
        "[Network] DHCP complete: IP={}, GW={:?}, DNS={:?}",
        ip,
        net_stack.gateway,
        net_stack.dns_server
    ));

    // 5. Store in global
    NETWORK_STACK.call_once(|| Mutex::new(net_stack));
    NET_READY.call_once(|| ());

    Ok(())
}

/// Check if the network stack is initialized and ready.
pub fn is_ready() -> bool {
    NET_READY.is_completed()
}

/// Get a reference to the global network stack.
///
/// Returns None if init() hasn't been called or failed. The returned
/// Mutex must be locked before use. Used by the inference engine to
/// make HTTP requests to cloud AI APIs.
pub fn get_stack() -> Option<&'static Mutex<NetworkStack>> {
    NETWORK_STACK.get()
}

// Re-export the log crate's macros so our modules can use println!-style
// logging without depending on the kernel's display module directly.
// The kernel sets up a log_fn callback that routes to its framebuffer.

static LOG_FN: spin::Once<fn(&str)> = spin::Once::new();

/// Set the logging function (called by kernel at init time).
pub fn set_log_fn(f: fn(&str)) {
    LOG_FN.call_once(|| f);
}

/// Internal println! macro that routes to the kernel's display.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use alloc::format;
        let msg = format!($($arg)*);
        if let Some(f) = $crate::LOG_FN.get() {
            f(&msg);
        }
    }};
}
