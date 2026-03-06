//! PCI bus scanning and configuration space access.
//!
//! Provides helpers for reading/writing PCI config registers and a unified
//! scanner that detects all supported NIC types: virtio-net, Intel e1000e,
//! and Realtek RTL8139.

use x86_64::instructions::port::Port;

/// Identifies a PCI device on the bus.
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub bar0: u32,
}

/// Supported NIC families.
#[derive(Debug, Clone, Copy)]
pub enum NicKind {
    VirtioNet,
    IntelE1000,
    RealtekRtl8139,
}

/// Intel e1000/e1000e device IDs — covers legacy e1000 (QEMU) and
/// modern I217/I218/I219 (Haswell through Alder Lake and beyond).
const E1000_DEVICES: &[u16] = &[
    // Legacy e1000 (QEMU, older servers)
    0x100E, // 82540EM (QEMU default e1000)
    0x100F, // 82545EM
    0x10D3, // 82574L
    // I217 (Haswell)
    0x153A, // I217-LM
    0x153B, // I217-V
    // I218 (Broadwell)
    0x1559, // I218-V
    0x155A, // I218-LM
    0x15A0, // I218-LM (3)
    0x15A1, // I218-V (3)
    0x15A2, // I218-LM (2)
    0x15A3, // I218-V (2)
    // I219 (Skylake through Alder Lake+)
    0x15B7, // I219-LM
    0x15B8, // I219-V
    0x15B9, // I219-LM (2)
    0x15BD, // I219-LM (4)
    0x15BE, // I219-V (4)
    0x15D6, // I219-V (5)
    0x15D7, // I219-LM (3)
    0x15D8, // I219-V (3)
    0x15E3, // I219-LM (5)
    0x15FA, // I219-LM (10)
    0x15FB, // I219-V (10)
    0x15FC, // I219-LM (11)
    0x15FD, // I219-V (11)
    0x0D4E, // I219-LM (12)
    0x0D4F, // I219-V (12)
    0x0D4C, // I219-LM (13)
    0x0D4D, // I219-V (13)
    0x0D53, // I219-LM (14)
    0x0D55, // I219-V (14)
    0x1A1E, // I219-LM (15)
    0x1A1F, // I219-V (15)
    0x1A1C, // I219-LM (16)
    0x1A1D, // I219-V (16)
    0x550A, // I219-LM (17)
    0x550B, // I219-V (17)
];

/// Scan PCI bus 0 for the first supported NIC. Returns the PCI device
/// info and the NIC type, or None if no supported NIC is found.
pub fn find_nic() -> Option<(PciDevice, NicKind)> {
    for slot in 0..32u8 {
        let vendor = pci_read_u16(0, slot, 0, 0x00);
        if vendor == 0xFFFF {
            continue;
        }
        let device = pci_read_u16(0, slot, 0, 0x02);
        let bar0 = pci_read_u32(0, slot, 0, 0x10);

        let pci = PciDevice {
            bus: 0,
            slot,
            func: 0,
            vendor_id: vendor,
            device_id: device,
            bar0,
        };

        // Virtio-net: Red Hat vendor, legacy or modern device ID
        if vendor == 0x1AF4 && (device == 0x1000 || device == 0x1041) {
            enable_bus_mastering(0, slot, 0);
            return Some((pci, NicKind::VirtioNet));
        }

        // Intel e1000/e1000e
        if vendor == 0x8086 && E1000_DEVICES.contains(&device) {
            enable_bus_mastering(0, slot, 0);
            return Some((pci, NicKind::IntelE1000));
        }

        // Realtek RTL8139
        if vendor == 0x10EC && device == 0x8139 {
            enable_bus_mastering(0, slot, 0);
            return Some((pci, NicKind::RealtekRtl8139));
        }
    }

    None
}

/// Enable PCI bus mastering (bit 2 of command register). Required for DMA.
fn enable_bus_mastering(bus: u8, slot: u8, func: u8) {
    let cmd = pci_read_u16(bus, slot, func, 0x04);
    pci_write_u16(bus, slot, func, 0x04, cmd | 0x04);
}

// --- PCI configuration space access via I/O ports 0xCF8 / 0xCFC ---

pub(crate) fn pci_read_u16(bus: u8, dev: u8, func: u8, off: u8) -> u16 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        let data = Port::<u32>::new(0xCFC).read();
        if off & 2 == 0 {
            data as u16
        } else {
            (data >> 16) as u16
        }
    }
}

pub(crate) fn pci_read_u32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).read()
    }
}

pub(crate) fn pci_write_u16(bus: u8, dev: u8, func: u8, off: u8, val: u16) {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        let cur = Port::<u32>::new(0xCFC).read();
        let new = if off & 2 == 0 {
            (cur & 0xFFFF_0000) | (val as u32)
        } else {
            (cur & 0x0000_FFFF) | ((val as u32) << 16)
        };
        Port::<u32>::new(0xCFC).write(new);
    }
}
