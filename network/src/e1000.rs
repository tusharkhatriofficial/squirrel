//! Intel e1000/e1000e Ethernet driver.
//!
//! Supports the Intel 82540EM (QEMU e1000), 82574L, and the modern
//! I217/I218/I219 family found in nearly every Intel-based PC since 2012.
//!
//! This is an MMIO-based driver: registers are accessed via memory-mapped
//! I/O at the address specified by PCI BAR0. Since Limine provides a
//! Higher-Half Direct Map (HHDM), we access MMIO at `phys + HHDM_OFFSET`.

use alloc::{vec, vec::Vec};
use core::ptr;

use crate::nic::NicDriver;
use crate::pci::PciDevice;

const NUM_RX_DESC: usize = 32;
const NUM_TX_DESC: usize = 32;
const RX_BUF_SIZE: usize = 2048;

// e1000 register offsets (MMIO)
const REG_CTRL:  u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EERD:  u32 = 0x0014;
const REG_ICR:   u32 = 0x00C0;
const REG_IMC:   u32 = 0x00D8;
const REG_RCTL:  u32 = 0x0100;
const REG_TCTL:  u32 = 0x0400;
const REG_RDBAL: u32 = 0x2800;
const REG_RDBAH: u32 = 0x2804;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH:   u32 = 0x2810;
const REG_RDT:   u32 = 0x2818;
const REG_TDBAL: u32 = 0x3800;
const REG_TDBAH: u32 = 0x3804;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH:   u32 = 0x3810;
const REG_TDT:   u32 = 0x3818;
const REG_RAL:   u32 = 0x5400;
const REG_RAH:   u32 = 0x5404;

// CTRL bits
const CTRL_SLU: u32 = 1 << 6;   // Set link up
const CTRL_RST: u32 = 1 << 26;  // Device reset

// RCTL bits
const RCTL_EN:    u32 = 1 << 1;   // Receiver enable
const RCTL_BAM:   u32 = 1 << 15;  // Broadcast accept
const RCTL_SECRC: u32 = 1 << 26;  // Strip CRC

// TCTL bits
const TCTL_EN:  u32 = 1 << 1;  // Transmitter enable
const TCTL_PSP: u32 = 1 << 3;  // Pad short packets

// Descriptor status/command bits
const RXDESC_DD:   u8 = 1 << 0;  // Descriptor done
const TXDESC_EOP:  u8 = 1 << 0;  // End of packet
const TXDESC_IFCS: u8 = 1 << 1;  // Insert FCS (CRC)
const TXDESC_RS:   u8 = 1 << 3;  // Report status
const TXDESC_DD:   u8 = 1 << 0;  // Descriptor done

/// RX descriptor (16 bytes, hardware-defined).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDesc {
    addr:     u64,
    length:   u16,
    checksum: u16,
    status:   u8,
    errors:   u8,
    special:  u16,
}

/// TX descriptor (16 bytes, hardware-defined).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDesc {
    addr:    u64,
    length:  u16,
    cso:     u8,
    cmd:     u8,
    status:  u8,
    css:     u8,
    special: u16,
}

pub struct E1000 {
    /// Virtual address of MMIO registers (BAR0 phys + HHDM offset)
    mmio_base: usize,
    pub mac: [u8; 6],

    rx_descs: Vec<RxDesc>,
    rx_buffers: Vec<Vec<u8>>,
    rx_cur: usize,

    tx_descs: Vec<TxDesc>,
    tx_buffers: Vec<Vec<u8>>,
    tx_cur: usize,
}

unsafe impl Send for E1000 {}

impl E1000 {
    pub fn new(pci: &PciDevice, hhdm_offset: u64) -> Self {
        // BAR0 bit 0 = 0 means MMIO (memory-mapped), not I/O port
        let bar0_phys = (pci.bar0 & !0xF) as u64;
        // Access via HHDM: Limine maps all physical memory at phys + hhdm_offset
        let mmio_base = (bar0_phys + hhdm_offset) as usize;

        let mut dev = Self {
            mmio_base,
            mac: [0; 6],
            rx_descs: vec![unsafe { core::mem::zeroed() }; NUM_RX_DESC],
            rx_buffers: Vec::new(),
            rx_cur: 0,
            tx_descs: vec![unsafe { core::mem::zeroed() }; NUM_TX_DESC],
            tx_buffers: Vec::new(),
            tx_cur: 0,
        };

        dev.reset();
        dev.read_mac();
        dev.init_rx(hhdm_offset);
        dev.init_tx(hhdm_offset);

        dev
    }

    fn read_reg(&self, offset: u32) -> u32 {
        unsafe {
            let ptr = (self.mmio_base + offset as usize) as *const u32;
            ptr::read_volatile(ptr)
        }
    }

    fn write_reg(&self, offset: u32, val: u32) {
        unsafe {
            let ptr = (self.mmio_base + offset as usize) as *mut u32;
            ptr::write_volatile(ptr, val);
        }
    }

    fn reset(&self) {
        // Disable interrupts
        self.write_reg(REG_IMC, 0xFFFF_FFFF);

        // Reset
        let ctrl = self.read_reg(REG_CTRL);
        self.write_reg(REG_CTRL, ctrl | CTRL_RST);

        // Wait for reset (~1ms worth of spin)
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }

        // Disable interrupts again (reset may re-enable)
        self.write_reg(REG_IMC, 0xFFFF_FFFF);
        self.read_reg(REG_ICR); // clear pending

        // Set link up
        let ctrl = self.read_reg(REG_CTRL);
        self.write_reg(REG_CTRL, ctrl | CTRL_SLU);
    }

    fn read_mac(&mut self) {
        // Try RAL/RAH first (most e1000e variants store MAC here)
        let ral = self.read_reg(REG_RAL);
        let rah = self.read_reg(REG_RAH);

        if ral != 0 && (rah & 0x8000_0000 != 0 || rah != 0) {
            self.mac[0] = (ral >> 0) as u8;
            self.mac[1] = (ral >> 8) as u8;
            self.mac[2] = (ral >> 16) as u8;
            self.mac[3] = (ral >> 24) as u8;
            self.mac[4] = (rah >> 0) as u8;
            self.mac[5] = (rah >> 8) as u8;

            // If we got a valid MAC (not all zeros), we're done
            if self.mac.iter().any(|&b| b != 0) {
                return;
            }
        }

        // Fallback: read from EEPROM
        for i in 0u8..3 {
            let word = self.eeprom_read(i);
            self.mac[i as usize * 2] = word as u8;
            self.mac[i as usize * 2 + 1] = (word >> 8) as u8;
        }

        // Write MAC to RAL/RAH so hardware uses it for filtering
        let ral_val = (self.mac[0] as u32)
            | ((self.mac[1] as u32) << 8)
            | ((self.mac[2] as u32) << 16)
            | ((self.mac[3] as u32) << 24);
        let rah_val = (self.mac[4] as u32)
            | ((self.mac[5] as u32) << 8)
            | (1u32 << 31); // Address valid bit
        self.write_reg(REG_RAL, ral_val);
        self.write_reg(REG_RAH, rah_val);
    }

    fn eeprom_read(&self, addr: u8) -> u16 {
        // Write address + start bit to EERD
        self.write_reg(REG_EERD, ((addr as u32) << 8) | 1);
        // Wait for done bit (bit 4)
        loop {
            let val = self.read_reg(REG_EERD);
            if val & (1 << 4) != 0 {
                return (val >> 16) as u16;
            }
            core::hint::spin_loop();
        }
    }

    fn init_rx(&mut self, hhdm_offset: u64) {
        for i in 0..NUM_RX_DESC {
            let buf = vec![0u8; RX_BUF_SIZE];
            let phys = virt_to_phys(buf.as_ptr() as u64, hhdm_offset);
            self.rx_descs[i].addr = phys;
            self.rx_descs[i].status = 0;
            self.rx_buffers.push(buf);
        }

        let descs_phys = virt_to_phys(self.rx_descs.as_ptr() as u64, hhdm_offset);
        self.write_reg(REG_RDBAL, descs_phys as u32);
        self.write_reg(REG_RDBAH, (descs_phys >> 32) as u32);
        self.write_reg(REG_RDLEN, (NUM_RX_DESC * 16) as u32);
        self.write_reg(REG_RDH, 0);
        self.write_reg(REG_RDT, (NUM_RX_DESC - 1) as u32);

        // Enable receiver: accept broadcast, strip CRC, 2048-byte buffers
        self.write_reg(REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
    }

    fn init_tx(&mut self, hhdm_offset: u64) {
        for i in 0..NUM_TX_DESC {
            let buf = vec![0u8; RX_BUF_SIZE];
            let phys = virt_to_phys(buf.as_ptr() as u64, hhdm_offset);
            self.tx_descs[i].addr = phys;
            self.tx_descs[i].status = TXDESC_DD; // Mark as done (available)
            self.tx_buffers.push(buf);
        }

        let descs_phys = virt_to_phys(self.tx_descs.as_ptr() as u64, hhdm_offset);
        self.write_reg(REG_TDBAL, descs_phys as u32);
        self.write_reg(REG_TDBAH, (descs_phys >> 32) as u32);
        self.write_reg(REG_TDLEN, (NUM_TX_DESC * 16) as u32);
        self.write_reg(REG_TDH, 0);
        self.write_reg(REG_TDT, 0);

        // Enable transmitter: pad short packets, collision settings
        self.write_reg(REG_TCTL, TCTL_EN | TCTL_PSP
            | (15 << 4)   // Collision threshold
            | (64 << 12)  // Collision distance
        );
    }
}

impl NicDriver for E1000 {
    fn send_frame(&mut self, frame: &[u8]) {
        let idx = self.tx_cur;

        // Wait for descriptor to be available
        if self.tx_descs[idx].status & TXDESC_DD == 0 {
            return; // TX ring full — drop frame
        }

        // Copy frame into TX buffer
        let len = frame.len().min(1514);
        self.tx_buffers[idx][..len].copy_from_slice(&frame[..len]);

        // Descriptor addr already points to the buffer's physical address.
        // Update length and command.
        self.tx_descs[idx].length = len as u16;
        self.tx_descs[idx].cmd = TXDESC_EOP | TXDESC_IFCS | TXDESC_RS;
        self.tx_descs[idx].status = 0;

        // Advance tail pointer — tells hardware there's a new frame
        self.tx_cur = (idx + 1) % NUM_TX_DESC;
        self.write_reg(REG_TDT, self.tx_cur as u32);
    }

    fn recv_frame(&mut self, buf: &mut [u8]) -> Option<usize> {
        let idx = self.rx_cur;

        // Check if descriptor has been filled by hardware
        if self.rx_descs[idx].status & RXDESC_DD == 0 {
            return None;
        }

        let len = self.rx_descs[idx].length as usize;
        let copy_len = len.min(buf.len());
        buf[..copy_len].copy_from_slice(&self.rx_buffers[idx][..copy_len]);

        // Reset descriptor for reuse
        self.rx_descs[idx].status = 0;

        let old_cur = self.rx_cur;
        self.rx_cur = (idx + 1) % NUM_RX_DESC;
        self.write_reg(REG_RDT, old_cur as u32);

        Some(copy_len)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

/// Convert a virtual (heap) address to a physical address for DMA.
///
/// Limine maps all physical memory at `phys + hhdm_offset`, so the
/// kernel heap (at 0xFFFF_FFFF_9000_0000+) sits in this HHDM region.
/// To get the physical address: `phys = virt - hhdm_offset`.
fn virt_to_phys(virt: u64, hhdm_offset: u64) -> u64 {
    virt - hhdm_offset
}
