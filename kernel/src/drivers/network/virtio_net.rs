//! Virtio-net driver stub — PCI device discovery for QEMU's virtio NIC.
//!
//! This module scans the PCI bus for a virtio-net device (vendor 0x1AF4,
//! device IDs 0x1000 for transitional or 0x1041 for modern). The actual
//! virtqueue TX/RX implementation is deferred to Phase 09 when the TCP/IP
//! stack needs a working NIC.
//!
//! QEMU must be launched with: -device virtio-net-pci,netdev=net0 -netdev user,id=net0

use super::{NetworkDevice, NetworkError};
use crate::println;

pub struct VirtioNetDevice {
    mac: [u8; 6],
    // Phase 09 fills in the real virtqueue implementation
}

impl VirtioNetDevice {
    /// Scan PCI bus 0 for a virtio-net device.
    /// Returns Some(device) if found, None otherwise.
    pub fn find() -> Option<Self> {
        for device in 0..32u8 {
            let vendor = pci_read_u16(0, device, 0, 0x00);
            if vendor == 0xFFFF {
                continue; // No device in this slot
            }
            let dev_id = pci_read_u16(0, device, 0, 0x02);
            if vendor == 0x1AF4 && (dev_id == 0x1000 || dev_id == 0x1041) {
                println!("[HW] virtio-net found at PCI 0:{}.0", device);
                // Return a placeholder MAC — Phase 09 reads it from device config
                return Some(Self {
                    mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
                });
            }
        }
        None
    }
}

impl NetworkDevice for VirtioNetDevice {
    fn send_frame(&mut self, _frame: &[u8]) -> Result<(), NetworkError> {
        Err(NetworkError::HardwareError) // Phase 09
    }
    fn recv_frame(&mut self, _buf: &mut [u8]) -> Result<usize, NetworkError> {
        Err(NetworkError::RxEmpty) // Phase 09
    }
    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

/// Read a 16-bit value from PCI configuration space using I/O ports.
///
/// PCI config space is accessed through port 0xCF8 (address) and 0xCFC (data).
/// The address format encodes bus, device, function, and register offset.
fn pci_read_u16(bus: u8, device: u8, func: u8, offset: u8) -> u16 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        use x86_64::instructions::port::Port;
        Port::<u32>::new(0xCF8).write(addr);
        let data = Port::<u32>::new(0xCFC).read();
        if offset & 2 == 0 {
            data as u16
        } else {
            (data >> 16) as u16
        }
    }
}
