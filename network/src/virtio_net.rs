//! Virtio-net driver — full virtqueue-based Ethernet for QEMU.
//!
//! This driver implements the virtio 1.0 legacy interface over PCI I/O ports.
//! QEMU's virtio-net device speaks this protocol when launched with:
//!   -device virtio-net-pci,netdev=net0 -netdev user,id=net0
//!
//! The driver manages two virtqueues:
//!   - RX queue (index 0): receives Ethernet frames from the network
//!   - TX queue (index 1): sends Ethernet frames to the network
//!
//! Each virtqueue is a ring buffer of descriptors pointing to DMA-accessible
//! memory buffers. The device reads/writes these buffers directly.

use alloc::{vec, vec::Vec};
use core::ptr;
use x86_64::instructions::port::Port;

use crate::nic::NicDriver;

/// Allocate physically contiguous DMA memory from the kernel's DMA pool.
/// Returns (virtual_address, physical_address).
fn dma_alloc(size: usize, align: usize) -> (u64, u64) {
    extern "Rust" {
        fn kernel_dma_alloc(size: usize, align: usize) -> (u64, u64);
    }
    let (v, p) = unsafe { kernel_dma_alloc(size, align) };
    assert!(v != 0, "DMA allocation failed (requested {} bytes)", size);
    (v, p)
}

/// Convert a DMA virtual address to physical.
fn dma_virt_to_phys(virt: u64) -> u64 {
    extern "Rust" {
        fn kernel_dma_virt_to_phys(virt: u64) -> u64;
    }
    unsafe { kernel_dma_virt_to_phys(virt) }
}

// --- Virtio legacy interface register offsets (I/O space) ---
const REG_DEVICE_FEATURES: u16 = 0x00; // R:  device feature bits
const REG_DRIVER_FEATURES: u16 = 0x04; // W:  driver-accepted features
const REG_QUEUE_ADDR:      u16 = 0x08; // RW: queue physical page number
const REG_QUEUE_SIZE:      u16 = 0x0C; // R:  max queue size
const REG_QUEUE_SELECT:    u16 = 0x0E; // W:  select which queue
const REG_QUEUE_NOTIFY:    u16 = 0x10; // W:  notify device about queue
const REG_DEVICE_STATUS:   u16 = 0x12; // RW: device status
const REG_ISR_STATUS:      u16 = 0x13; // R:  interrupt status
const REG_MAC_BASE:        u16 = 0x14; // R:  6-byte MAC address

// --- Virtio device status bits ---
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;

// --- Virtio feature bits ---
const VIRTIO_NET_F_MAC: u32 = 1 << 5; // Device has a MAC address

// --- Virtqueue descriptor flags ---
const VRING_DESC_F_NEXT:  u16 = 1; // Buffer continues via `next` field
const VRING_DESC_F_WRITE: u16 = 2; // Buffer is device-writable (for RX)

// --- Constants ---
const QUEUE_SIZE: usize = 64;
const RX_BUF_SIZE: usize = 1514 + 12; // Max Ethernet frame + virtio-net header
const VIRTIO_NET_HDR_SIZE: usize = 12; // Legacy virtio-net header (no mergeable bufs)

/// A single virtqueue descriptor (16 bytes, as defined by virtio spec).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct VringDesc {
    addr:  u64, // Physical address of the buffer
    len:   u32, // Length of the buffer
    flags: u16, // VRING_DESC_F_* flags
    next:  u16, // Index of next descriptor (if F_NEXT set)
}

/// The "available" ring — driver writes here to offer buffers to device.
#[repr(C, align(2))]
struct VringAvail {
    flags: u16,
    idx:   u16,                    // Next index the driver will write
    ring:  [u16; QUEUE_SIZE],      // Descriptor chain heads
}

/// One entry in the "used" ring — device writes here when done with a buffer.
#[repr(C)]
#[derive(Clone, Copy)]
struct VringUsedElem {
    id:  u32, // Index of the descriptor chain head
    len: u32, // Total bytes written by device
}

/// The "used" ring — device writes here to return consumed buffers.
#[repr(C, align(4))]
struct VringUsed {
    flags: u16,
    idx:   u16,                         // Next index the device will write
    ring:  [VringUsedElem; QUEUE_SIZE],
}

/// A complete virtqueue: descriptor table + available ring + used ring.
/// All three are in physically contiguous DMA memory.
struct Virtqueue {
    descs: *mut VringDesc,
    avail: *mut VringAvail,
    used:  *mut VringUsed,
    /// Physical address of the descriptor table base (for REG_QUEUE_ADDR).
    phys_base: u64,
    /// Next free descriptor index
    free_head: u16,
    /// Number of free descriptors
    num_free: u16,
    /// Last seen used index (driver-side tracking)
    last_used_idx: u16,
}

impl Virtqueue {
    /// Allocate and initialize a virtqueue in DMA-safe memory.
    ///
    /// Uses the kernel's DMA pool which provides physically contiguous
    /// memory that devices can safely read/write via DMA.
    fn new() -> Self {
        // Calculate sizes for each section
        let desc_size  = QUEUE_SIZE * core::mem::size_of::<VringDesc>();
        let avail_size = 4 + QUEUE_SIZE * 2 + 2; // flags(2) + idx(2) + ring + used_event(2)
        let used_size  = 4 + QUEUE_SIZE * core::mem::size_of::<VringUsedElem>() + 2;

        // Used ring must be page-aligned per virtio spec, so allocate enough
        // that we can place used ring at a 4096-byte boundary within the block.
        let used_offset = (desc_size + avail_size + 4095) & !4095;
        let total = used_offset + used_size;

        // Allocate from DMA pool (page-aligned, physically contiguous)
        let (virt, phys) = dma_alloc(total, 4096);

        let desc_virt = virt as usize;
        let avail_virt = desc_virt + desc_size;
        let used_virt = desc_virt + used_offset;

        let descs = desc_virt as *mut VringDesc;
        let avail = avail_virt as *mut VringAvail;
        let used  = used_virt as *mut VringUsed;

        // Initialize descriptor free list: each desc points to next
        for i in 0..(QUEUE_SIZE as u16) {
            unsafe {
                let d = &mut *descs.add(i as usize);
                d.addr = 0;
                d.len = 0;
                d.flags = 0;
                d.next = if (i as usize) < QUEUE_SIZE - 1 { i + 1 } else { 0 };
            }
        }

        // Zero out avail and used rings
        unsafe {
            ptr::write_bytes(avail, 0, 1);
            ptr::write_bytes(used, 0, 1);
        }

        Virtqueue {
            descs,
            avail,
            used,
            phys_base: phys,
            free_head: 0,
            num_free: QUEUE_SIZE as u16,
            last_used_idx: 0,
        }
    }

    /// Physical page number of the descriptor table (for REG_QUEUE_ADDR).
    fn page_number(&self) -> u32 {
        (self.phys_base / 4096) as u32
    }

    /// Allocate one descriptor from the free list.
    fn alloc_desc(&mut self) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }
        let idx = self.free_head;
        unsafe {
            self.free_head = (*self.descs.add(idx as usize)).next;
        }
        self.num_free -= 1;
        Some(idx)
    }

    /// Return a descriptor to the free list.
    fn free_desc(&mut self, idx: u16) {
        unsafe {
            let d = &mut *self.descs.add(idx as usize);
            d.flags = 0;
            d.next = self.free_head;
        }
        self.free_head = idx;
        self.num_free += 1;
    }

    /// Push a descriptor chain head into the available ring.
    fn push_avail(&mut self, desc_idx: u16) {
        unsafe {
            let avail = &mut *self.avail;
            let ring_idx = avail.idx as usize % QUEUE_SIZE;
            avail.ring[ring_idx] = desc_idx;
            // Memory barrier: ensure descriptor is visible before updating index
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            avail.idx = avail.idx.wrapping_add(1);
        }
    }

    /// Pop completed entries from the used ring.
    /// Returns (descriptor_chain_head, bytes_written).
    fn pop_used(&mut self) -> Option<(u16, u32)> {
        unsafe {
            let used = &*self.used;
            if self.last_used_idx == used.idx {
                return None;
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            let ring_idx = self.last_used_idx as usize % QUEUE_SIZE;
            let elem = used.ring[ring_idx];
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            Some((elem.id as u16, elem.len))
        }
    }
}

// PCI scanning moved to pci.rs — use pci::find_nic() instead.

/// Per-RX-buffer tracking: virtual + physical addresses for DMA.
struct RxBufEntry {
    virt: u64,
    phys: u64,
}

/// The virtio-net device driver.
///
/// Manages two virtqueues (RX and TX) and provides send/recv for raw
/// Ethernet frames. The smoltcp adapter in `stack.rs` wraps this to
/// provide TCP/IP networking.
pub struct VirtioNet {
    io_base: u16,
    pub mac: [u8; 6],
    rx_queue: Virtqueue,
    tx_queue: Virtqueue,
    /// Pre-allocated RX buffers in DMA memory (one per descriptor)
    rx_buffers: Vec<RxBufEntry>,
}

// Safety: VirtioNet is only accessed from one context at a time (the
// NetworkAgent runs single-threaded within SART's cooperative scheduler).
// The raw pointers in Virtqueue point to heap memory owned by the struct.
unsafe impl Send for VirtioNet {}

impl VirtioNet {
    /// Initialize the virtio-net device at the given I/O base address.
    ///
    /// This performs the full virtio handshake:
    /// 1. Reset device
    /// 2. Set ACKNOWLEDGE + DRIVER status
    /// 3. Negotiate features (we only need MAC)
    /// 4. Set up RX and TX virtqueues
    /// 5. Read MAC address
    /// 6. Set DRIVER_OK — device is live
    pub fn new(io_base: u16) -> Self {
        let mut dev = Self {
            io_base,
            mac: [0u8; 6],
            rx_queue: Virtqueue::new(),
            tx_queue: Virtqueue::new(),
            rx_buffers: Vec::new(),
        };
        dev.init();
        dev
    }

    // --- I/O port helpers ---
    fn io_read8(&self, off: u16) -> u8 {
        unsafe { Port::<u8>::new(self.io_base + off).read() }
    }
    fn io_write8(&self, off: u16, v: u8) {
        unsafe { Port::<u8>::new(self.io_base + off).write(v); }
    }
    fn io_write16(&self, off: u16, v: u16) {
        unsafe { Port::<u16>::new(self.io_base + off).write(v); }
    }
    fn io_read32(&self, off: u16) -> u32 {
        unsafe { Port::<u32>::new(self.io_base + off).read() }
    }
    fn io_write32(&self, off: u16, v: u32) {
        unsafe { Port::<u32>::new(self.io_base + off).write(v); }
    }

    fn init(&mut self) {
        // 1. Reset device
        self.io_write8(REG_DEVICE_STATUS, 0);

        // 2. Acknowledge + driver
        self.io_write8(REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
        self.io_write8(REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // 3. Negotiate features: we want MAC address (bit 5)
        let features = self.io_read32(REG_DEVICE_FEATURES);
        self.io_write32(REG_DRIVER_FEATURES, features & VIRTIO_NET_F_MAC);

        // 4a. Set up RX queue (index 0)
        self.io_write16(REG_QUEUE_SELECT, 0);
        self.io_write32(REG_QUEUE_ADDR, self.rx_queue.page_number());

        // 4b. Set up TX queue (index 1)
        self.io_write16(REG_QUEUE_SELECT, 1);
        self.io_write32(REG_QUEUE_ADDR, self.tx_queue.page_number());

        // 5. Read MAC address from device config space
        for i in 0..6 {
            self.mac[i] = self.io_read8(REG_MAC_BASE + i as u16);
        }

        // 6. Set DRIVER_OK — device is now live
        self.io_write8(REG_DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

        // 7. Fill the RX queue with empty buffers so the device can write into them
        self.fill_rx_queue();
    }

    /// Pre-allocate RX buffers in DMA memory and submit them to the RX virtqueue.
    /// The device will write received Ethernet frames into these buffers.
    fn fill_rx_queue(&mut self) {
        for _ in 0..QUEUE_SIZE {
            let (virt, phys) = dma_alloc(RX_BUF_SIZE, 16);
            self.rx_buffers.push(RxBufEntry { virt, phys });

            if let Some(desc_idx) = self.rx_queue.alloc_desc() {
                unsafe {
                    let d = &mut *self.rx_queue.descs.add(desc_idx as usize);
                    d.addr = phys;
                    d.len = RX_BUF_SIZE as u32;
                    d.flags = VRING_DESC_F_WRITE; // Device writes into this buffer
                    d.next = 0;
                }
                self.rx_queue.push_avail(desc_idx);
            }
        }
        // Notify device that RX buffers are available
        self.io_write16(REG_QUEUE_NOTIFY, 0);
    }

    /// Send an Ethernet frame.
    ///
    /// Prepends the 12-byte virtio-net header (all zeros = no offloading),
    /// copies the frame into a DMA TX buffer, submits it to the TX virtqueue,
    /// and notifies the device.
    pub fn send_raw_frame(&mut self, frame: &[u8]) {
        // Reclaim any completed TX descriptors first
        while let Some((desc_idx, _)) = self.tx_queue.pop_used() {
            self.tx_queue.free_desc(desc_idx);
        }

        let desc_idx = match self.tx_queue.alloc_desc() {
            Some(idx) => idx,
            None => return, // TX ring full, drop the frame
        };

        // Allocate TX buffer from DMA pool
        let total_len = VIRTIO_NET_HDR_SIZE + frame.len();
        let (tx_virt, tx_phys) = dma_alloc(total_len, 16);

        // Build the TX buffer: virtio-net header (zeros) + Ethernet frame
        unsafe {
            ptr::write_bytes(tx_virt as *mut u8, 0, VIRTIO_NET_HDR_SIZE);
            ptr::copy_nonoverlapping(
                frame.as_ptr(),
                (tx_virt as *mut u8).add(VIRTIO_NET_HDR_SIZE),
                frame.len(),
            );

            let d = &mut *self.tx_queue.descs.add(desc_idx as usize);
            d.addr = tx_phys;
            d.len = total_len as u32;
            d.flags = 0; // Device reads this buffer (no WRITE flag)
            d.next = 0;
        }

        self.tx_queue.push_avail(desc_idx);
        self.io_write16(REG_QUEUE_NOTIFY, 1); // Notify TX queue
    }

    /// Receive a pending Ethernet frame (non-blocking).
    ///
    /// Checks the RX used ring for completed buffers. If a frame is available,
    /// strips the virtio-net header, copies the Ethernet frame into `buf`,
    /// and returns the number of bytes. Re-submits the buffer for reuse.
    pub fn recv_raw_frame(&mut self, buf: &mut [u8]) -> Option<usize> {
        let (desc_idx, total_len) = self.rx_queue.pop_used()?;

        // The device wrote total_len bytes including the virtio-net header
        let frame_len = total_len as usize - VIRTIO_NET_HDR_SIZE;
        if frame_len > buf.len() {
            // Frame too large for caller's buffer — re-submit and skip
            self.resubmit_rx(desc_idx);
            return None;
        }

        // Copy the Ethernet frame (skip the 12-byte virtio-net header)
        let entry = &self.rx_buffers[desc_idx as usize];
        unsafe {
            let src = (entry.virt as *const u8).add(VIRTIO_NET_HDR_SIZE);
            ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), frame_len);
        }

        // Re-submit this buffer to the RX queue for the next frame
        self.resubmit_rx(desc_idx);
        Some(frame_len)
    }

    /// Re-submit an RX buffer to the device for reuse.
    fn resubmit_rx(&mut self, desc_idx: u16) {
        let entry = &self.rx_buffers[desc_idx as usize];
        unsafe {
            let d = &mut *self.rx_queue.descs.add(desc_idx as usize);
            d.addr = entry.phys;
            d.len = RX_BUF_SIZE as u32;
            d.flags = VRING_DESC_F_WRITE;
            d.next = 0;
        }
        self.rx_queue.push_avail(desc_idx);
        self.io_write16(REG_QUEUE_NOTIFY, 0); // Notify RX queue
    }
}

impl NicDriver for VirtioNet {
    fn send_frame(&mut self, frame: &[u8]) {
        VirtioNet::send_raw_frame(self, frame);
    }

    fn recv_frame(&mut self, buf: &mut [u8]) -> Option<usize> {
        VirtioNet::recv_raw_frame(self, buf)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

// PCI helpers are in pci.rs — use crate::pci::* for PCI access.
