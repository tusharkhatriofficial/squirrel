//! DMA allocator — physically contiguous memory for device I/O.
//!
//! Network cards (virtio-net, e1000, rtl8139) need DMA buffers where
//! contiguous virtual addresses map to contiguous physical addresses.
//! The normal heap can't guarantee this because each page is allocated
//! independently from the PMM.
//!
//! This module reserves a contiguous physical region at boot and manages
//! it with a simple bump/free-list allocator. All DMA allocations come
//! from this pool.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, Once};
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::memory::{pmm::PhysicalMemoryManager, vmm::VirtualMemoryManager};

/// DMA pool size: 2 MB (512 pages). Enough for virtio-net's two virtqueues
/// (descriptor tables, avail/used rings) plus 64 RX buffers and TX staging.
const DMA_POOL_PAGES: usize = 512;
const DMA_POOL_SIZE: usize = DMA_POOL_PAGES * 4096;

/// Virtual address base for DMA region (right after the heap).
const DMA_VIRT_BASE: u64 = 0xFFFF_FFFF_9800_0000;

/// Global DMA allocator instance.
pub static DMA: Once<Mutex<DmaAllocator>> = Once::new();

/// The physical base address of the DMA pool (for virt→phys conversion).
static DMA_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

/// A simple bump allocator over a physically contiguous region.
///
/// Allocations are rounded up to 4KB alignment. Free is supported but
/// only reclaims memory when the entire pool is freed (bump reset).
/// For the MVP this is fine — DMA buffers are allocated once at driver
/// init and never freed.
pub struct DmaAllocator {
    /// Next free offset within the pool (bump pointer).
    offset: usize,
    /// Total pool size in bytes.
    size: usize,
}

impl DmaAllocator {
    fn new(size: usize) -> Self {
        Self { offset: 0, size }
    }

    /// Allocate `size` bytes from the DMA pool.
    /// Returns (virtual_address, physical_address) or None if out of space.
    /// All allocations are aligned to `align` bytes (minimum 16).
    pub fn alloc(&mut self, size: usize, align: usize) -> Option<(u64, u64)> {
        let align = align.max(16);
        // Round offset up to alignment
        let aligned_offset = (self.offset + align - 1) & !(align - 1);
        if aligned_offset + size > self.size {
            return None;
        }
        let virt = DMA_VIRT_BASE + aligned_offset as u64;
        let phys = DMA_PHYS_BASE.load(Ordering::Relaxed) + aligned_offset as u64;
        self.offset = aligned_offset + size;
        Some((virt, phys))
    }

    /// How many bytes remain in the pool.
    pub fn remaining(&self) -> usize {
        self.size - self.offset
    }
}

/// Initialize the DMA pool: allocate contiguous physical pages from PMM,
/// map them into virtual address space, and set up the bump allocator.
pub fn init(
    vmm: &Mutex<VirtualMemoryManager>,
    pmm: &Mutex<PhysicalMemoryManager>,
) {
    // 1. Allocate physically contiguous frames from PMM
    let phys_base = pmm
        .lock()
        .allocate_contiguous(DMA_POOL_PAGES)
        .expect("DMA: cannot allocate contiguous physical memory");

    DMA_PHYS_BASE.store(phys_base.as_u64(), Ordering::Relaxed);

    // 2. Map each page into the DMA virtual range
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE;

    for i in 0..DMA_POOL_PAGES {
        let virt = VirtAddr::new(DMA_VIRT_BASE + (i * 4096) as u64);
        let phys_frame = x86_64::structures::paging::PhysFrame::containing_address(
            x86_64::PhysAddr::new(phys_base.as_u64() + (i * 4096) as u64),
        );
        vmm.lock()
            .map_page(virt, phys_frame, flags, pmm)
            .expect("DMA: page mapping failed");
    }

    // 3. Zero out the DMA region
    unsafe {
        core::ptr::write_bytes(DMA_VIRT_BASE as *mut u8, 0, DMA_POOL_SIZE);
    }

    // 4. Create the allocator
    DMA.call_once(|| Mutex::new(DmaAllocator::new(DMA_POOL_SIZE)));
}

/// Allocate DMA memory. Returns (virtual_ptr, physical_addr).
/// Align defaults to 4096 (page-aligned) which satisfies virtio spec.
pub fn dma_alloc(size: usize, align: usize) -> Option<(u64, u64)> {
    DMA.get()?.lock().alloc(size, align)
}

/// Convert a DMA virtual address to its physical address.
/// Only works for addresses within the DMA pool.
pub fn dma_virt_to_phys(virt: u64) -> u64 {
    let phys_base = DMA_PHYS_BASE.load(Ordering::Relaxed);
    if virt >= DMA_VIRT_BASE && virt < DMA_VIRT_BASE + DMA_POOL_SIZE as u64 {
        phys_base + (virt - DMA_VIRT_BASE)
    } else {
        0 // Not a DMA address
    }
}
