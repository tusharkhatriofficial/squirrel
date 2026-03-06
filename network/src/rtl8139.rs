//! Realtek RTL8139 Ethernet driver.
//!
//! The RTL8139 is one of the simplest common NICs. Uses I/O port access
//! (like virtio-net) and a simple contiguous ring buffer for RX.
//! Found in budget PCs, older laptops, and some embedded systems.

use alloc::{vec, vec::Vec};
use x86_64::instructions::port::Port;

use crate::nic::NicDriver;
use crate::pci::PciDevice;

const RX_BUF_SIZE: usize = 8192 + 16 + 1500; // 8K + header + overflow
const TX_BUF_SIZE: usize = 1536;

// Register offsets
const REG_MAC:     u16 = 0x00;
const REG_TSD0:    u16 = 0x10;  // TX status (4 descriptors, 4 bytes apart)
const REG_TSAD0:   u16 = 0x20;  // TX start address (4 descriptors)
const REG_RBSTART: u16 = 0x30;  // RX buffer start physical address
const REG_CMD:     u16 = 0x37;  // Command
const REG_CAPR:    u16 = 0x38;  // Current address of packet read
const REG_CBR:     u16 = 0x3A;  // Current buffer read
const REG_IMR:     u16 = 0x3C;  // Interrupt mask
const REG_ISR:     u16 = 0x3E;  // Interrupt status
const REG_RCR:     u16 = 0x44;  // RX config
const REG_CONFIG1: u16 = 0x52;  // Config 1

// CMD bits
const CMD_RST: u8 = 1 << 4;
const CMD_RE:  u8 = 1 << 3;  // Receiver enable
const CMD_TE:  u8 = 1 << 2;  // Transmitter enable
const CMD_BUFE: u8 = 1 << 0; // Buffer empty

// RCR bits
const RCR_APM:  u32 = 1 << 1;  // Accept physical match
const RCR_AM:   u32 = 1 << 2;  // Accept multicast
const RCR_AB:   u32 = 1 << 3;  // Accept broadcast
const RCR_WRAP: u32 = 1 << 7;  // Wrap at end of buffer

pub struct Rtl8139 {
    io_base: u16,
    pub mac: [u8; 6],
    rx_buffer: Vec<u8>,
    rx_offset: usize,
    tx_buffers: [Vec<u8>; 4],
    tx_cur: usize,
    /// HHDM offset for virt-to-phys conversion
    hhdm_offset: u64,
}

unsafe impl Send for Rtl8139 {}

impl Rtl8139 {
    pub fn new(pci: &PciDevice, hhdm_offset: u64) -> Self {
        // RTL8139 uses I/O space BAR (bit 0 = 1)
        let io_base = (pci.bar0 & !3) as u16;

        let mut dev = Self {
            io_base,
            mac: [0; 6],
            rx_buffer: vec![0u8; RX_BUF_SIZE],
            rx_offset: 0,
            tx_buffers: [
                vec![0u8; TX_BUF_SIZE],
                vec![0u8; TX_BUF_SIZE],
                vec![0u8; TX_BUF_SIZE],
                vec![0u8; TX_BUF_SIZE],
            ],
            tx_cur: 0,
            hhdm_offset,
        };

        dev.reset();
        dev.read_mac();
        dev.init();

        dev
    }

    fn read_u8(&self, offset: u16) -> u8 {
        unsafe { Port::new(self.io_base + offset).read() }
    }

    fn write_u8(&self, offset: u16, val: u8) {
        unsafe { Port::new(self.io_base + offset).write(val); }
    }

    fn read_u16(&self, offset: u16) -> u16 {
        unsafe { Port::new(self.io_base + offset).read() }
    }

    fn write_u16(&self, offset: u16, val: u16) {
        unsafe { Port::new(self.io_base + offset).write(val); }
    }

    fn write_u32(&self, offset: u16, val: u32) {
        unsafe { Port::new(self.io_base + offset).write(val); }
    }

    fn reset(&self) {
        // Power on
        self.write_u8(REG_CONFIG1, 0x00);

        // Software reset
        self.write_u8(REG_CMD, CMD_RST);

        // Wait for reset to complete (RST bit auto-clears)
        loop {
            if self.read_u8(REG_CMD) & CMD_RST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    fn read_mac(&mut self) {
        for i in 0..6 {
            self.mac[i] = self.read_u8(REG_MAC + i as u16);
        }
    }

    /// Convert virtual heap address to physical for DMA.
    fn virt_to_phys(&self, virt: u64) -> u32 {
        // RTL8139 uses 32-bit DMA addresses
        let phys = virt - self.hhdm_offset;
        phys as u32
    }

    fn init(&mut self) {
        // 1. Set RX buffer physical address
        let rx_phys = self.virt_to_phys(self.rx_buffer.as_ptr() as u64);
        self.write_u32(REG_RBSTART, rx_phys);

        // 2. Disable interrupts — we poll
        self.write_u16(REG_IMR, 0x0000);

        // 3. Configure RX: accept physical match + broadcast + multicast + wrap
        self.write_u32(REG_RCR, RCR_APM | RCR_AB | RCR_AM | RCR_WRAP);

        // 4. Enable RX and TX
        self.write_u8(REG_CMD, CMD_RE | CMD_TE);
    }
}

impl NicDriver for Rtl8139 {
    fn send_frame(&mut self, frame: &[u8]) {
        let idx = self.tx_cur;
        let len = frame.len().min(TX_BUF_SIZE);

        // Copy frame into TX buffer
        self.tx_buffers[idx][..len].copy_from_slice(&frame[..len]);

        // Set TX start address (physical)
        let tx_phys = self.virt_to_phys(self.tx_buffers[idx].as_ptr() as u64);
        self.write_u32(REG_TSAD0 + (idx as u16) * 4, tx_phys);

        // Set TX status — writing length starts transmission
        self.write_u32(REG_TSD0 + (idx as u16) * 4, len as u32);

        self.tx_cur = (idx + 1) % 4;
    }

    fn recv_frame(&mut self, buf: &mut [u8]) -> Option<usize> {
        // Check if RX buffer is empty
        let cmd = self.read_u8(REG_CMD);
        if cmd & CMD_BUFE != 0 {
            return None;
        }

        let offset = self.rx_offset % RX_BUF_SIZE;

        // Read the RTL8139 RX header (4 bytes: status u16 + length u16)
        let status = u16::from_le_bytes([
            self.rx_buffer[offset],
            self.rx_buffer[(offset + 1) % RX_BUF_SIZE],
        ]);
        let length = u16::from_le_bytes([
            self.rx_buffer[(offset + 2) % RX_BUF_SIZE],
            self.rx_buffer[(offset + 3) % RX_BUF_SIZE],
        ]) as usize;

        // Check if packet is valid (bit 0 = ROK)
        if status & 0x01 == 0 {
            // Bad packet — skip it
            return None;
        }

        if length < 4 || length > 1518 + 4 {
            // Invalid length — skip
            return None;
        }

        // Frame data starts after the 4-byte header
        let frame_len = length - 4; // Subtract CRC
        let copy_len = frame_len.min(buf.len());
        let data_start = offset + 4;

        for i in 0..copy_len {
            buf[i] = self.rx_buffer[(data_start + i) % RX_BUF_SIZE];
        }

        // Advance read pointer (aligned to 4 bytes + 4-byte header)
        self.rx_offset = (offset + length + 4 + 3) & !3;

        // Update CAPR — tells hardware we consumed this packet
        self.write_u16(REG_CAPR, (self.rx_offset as u16).wrapping_sub(16));

        // Clear interrupt status
        let isr = self.read_u16(REG_ISR);
        self.write_u16(REG_ISR, isr);

        Some(copy_len)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}
