//! Squirrel AIOS Network Stack
//!
//! Complete network stack for bare-metal x86_64: NIC drivers (virtio-net,
//! Intel e1000, Realtek RTL8139), smoltcp TCP/IP, and an HTTP/HTTPS client.
//! All wrapped in a SART agent that responds to Intent Bus messages.
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
//!   │  Box<dyn NicDriver>                 │  Any supported NIC
//!   ├──────────┬──────────┬───────────────┤
//!   │ VirtioNet│  E1000   │   RTL8139     │  Hardware drivers
//!   └──────────┴──────────┴───────────────┘
//!
//! TLS 1.3 is provided by embedded-tls (pure Rust, no C dependencies).

#![no_std]
extern crate alloc;

pub mod agent;
pub mod e1000;
pub mod http;
pub mod nic;
pub mod pci;
pub mod rng;
pub mod rtl8139;
pub mod stack;
pub mod tls;
pub mod virtio_net;

pub use agent::{NetworkAgent, NetworkRequest, NetworkResponse};
pub use stack::NetworkStack;

use pci::NicKind;
use spin::{Mutex, Once};

/// Global network stack instance.
///
/// Initialized by init() after DHCP completes. Wrapped in Once<Mutex<>>
/// so it's safe to access from multiple agents (the NetworkAgent polls it
/// every tick, and other agents can check network readiness).
static NETWORK_STACK: Once<Mutex<NetworkStack>> = Once::new();

/// Whether the network stack was initialized successfully.
static NET_READY: Once<()> = Once::new();

/// Initialize the network stack: scan PCI for a supported NIC, set up
/// the driver, run DHCP.
///
/// Called once from kernel main. The `hhdm_offset` is the Limine Higher-Half
/// Direct Map offset, needed by e1000 (MMIO) and rtl8139 (DMA addresses).
///
/// If no supported NIC is found, returns Err and the kernel continues
/// without networking.
pub fn init(log_fn: fn(&str), hhdm_offset: u64) -> Result<(), &'static str> {
    // 1. Scan PCI for any supported NIC
    let (pci_dev, kind) = pci::find_nic()
        .ok_or("No supported NIC found (need virtio-net, Intel e1000, or Realtek RTL8139)")?;

    let kind_name = match kind {
        NicKind::VirtioNet      => "virtio-net",
        NicKind::IntelE1000     => "Intel e1000",
        NicKind::RealtekRtl8139 => "Realtek RTL8139",
    };
    log_fn(&alloc::format!(
        "[Network] {} found at PCI 0:{}.0 (vendor={:04X} device={:04X})",
        kind_name, pci_dev.slot, pci_dev.vendor_id, pci_dev.device_id
    ));

    // 2. Create the appropriate driver
    let dev: alloc::boxed::Box<dyn nic::NicDriver> = match kind {
        NicKind::VirtioNet => {
            let io_base = (pci_dev.bar0 & !3) as u16;
            alloc::boxed::Box::new(virtio_net::VirtioNet::new(io_base))
        }
        NicKind::IntelE1000 => {
            alloc::boxed::Box::new(e1000::E1000::new(&pci_dev, hhdm_offset))
        }
        NicKind::RealtekRtl8139 => {
            alloc::boxed::Box::new(rtl8139::Rtl8139::new(&pci_dev, hhdm_offset))
        }
    };

    let mac = dev.mac_address();
    log_fn(&alloc::format!(
        "[Network] MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    ));

    // 3. Create the TCP/IP stack (same for all drivers)
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
