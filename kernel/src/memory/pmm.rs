use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

const FRAME_SIZE: usize = 4096;

pub struct PhysicalMemoryManager {
    /// One bit per 4KB frame. 0 = free, 1 = used.
    bitmap: &'static mut [u64],
    total_frames: usize,
    free_frames: usize,
    /// Optimization: last index we allocated at. Scan forward from here.
    next_free_hint: usize,
    hhdm_offset: u64,
}

impl PhysicalMemoryManager {
    pub fn new(mmap: &MemoryMapResponse, hhdm_offset: u64) -> Self {
        // Find the highest physical address to know how big the bitmap needs to be
        let highest_addr = mmap
            .entries()
            .iter()
            .map(|e| e.base + e.length)
            .max()
            .unwrap_or(0);

        let total_frames = (highest_addr as usize).div_ceil(FRAME_SIZE);
        let bitmap_bytes = total_frames.div_ceil(8);
        let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE);

        // Find a usable region big enough to hold the bitmap
        let bitmap_phys = mmap
            .entries()
            .iter()
            .find(|e| {
                e.entry_type == EntryType::USABLE
                    && e.length >= (bitmap_frames * FRAME_SIZE) as u64
            })
            .expect("no region large enough for PMM bitmap")
            .base;

        let bitmap_virt = (bitmap_phys + hhdm_offset) as *mut u64;
        let bitmap_len = total_frames.div_ceil(64); // number of u64 words

        // SAFETY: Limine guarantees this physical region is mapped via HHDM
        let bitmap = unsafe { core::slice::from_raw_parts_mut(bitmap_virt, bitmap_len) };

        // Mark everything used initially
        bitmap.fill(u64::MAX);

        let mut pmm = Self {
            bitmap,
            total_frames,
            free_frames: 0,
            next_free_hint: 0,
            hhdm_offset,
        };

        // Mark usable regions as free
        for entry in mmap
            .entries()
            .iter()
            .filter(|e| e.entry_type == EntryType::USABLE)
        {
            let start = entry.base as usize / FRAME_SIZE;
            let end = (entry.base + entry.length) as usize / FRAME_SIZE;
            for frame in start..end {
                pmm.set_free(frame);
                pmm.free_frames += 1;
            }
        }

        // Re-mark the bitmap region itself as used
        let bitmap_start = bitmap_phys as usize / FRAME_SIZE;
        for i in bitmap_start..(bitmap_start + bitmap_frames) {
            pmm.set_used(i);
            pmm.free_frames -= 1;
        }

        pmm
    }

    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    /// Allocate one 4KB physical frame. Returns physical address.
    pub fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let start = self.next_free_hint;
        for i in 0..self.total_frames {
            let idx = (start + i) % self.total_frames;
            if self.is_free(idx) {
                self.set_used(idx);
                self.free_frames -= 1;
                self.next_free_hint = (idx + 1) % self.total_frames;
                let phys = PhysAddr::new((idx * FRAME_SIZE) as u64);
                return Some(PhysFrame::containing_address(phys));
            }
        }
        None
    }

    /// Allocate `count` physically contiguous 4KB frames.
    /// Returns the physical address of the first frame, or None if no
    /// contiguous run of that size exists.
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<PhysAddr> {
        if count == 0 {
            return None;
        }
        // Scan the bitmap for a run of `count` consecutive free frames
        let mut run_start = 0;
        let mut run_len = 0;

        for idx in 0..self.total_frames {
            if self.is_free(idx) {
                if run_len == 0 {
                    run_start = idx;
                }
                run_len += 1;
                if run_len == count {
                    // Found a contiguous run — mark all frames as used
                    for i in run_start..(run_start + count) {
                        self.set_used(i);
                        self.free_frames -= 1;
                    }
                    return Some(PhysAddr::new((run_start * FRAME_SIZE) as u64));
                }
            } else {
                run_len = 0;
            }
        }
        None
    }

    /// Free a physical frame
    pub fn free_frame(&mut self, frame: PhysFrame) {
        let idx = frame.start_address().as_u64() as usize / FRAME_SIZE;
        assert!(idx < self.total_frames, "PMM: free_frame index out of range");
        assert!(!self.is_free(idx), "PMM: double-free detected");
        self.set_free(idx);
        self.free_frames += 1;
    }

    fn is_free(&self, idx: usize) -> bool {
        (self.bitmap[idx / 64] >> (idx % 64)) & 1 == 0
    }

    fn set_free(&mut self, idx: usize) {
        self.bitmap[idx / 64] &= !(1u64 << (idx % 64));
    }

    fn set_used(&mut self, idx: usize) {
        self.bitmap[idx / 64] |= 1u64 << (idx % 64);
    }
}
