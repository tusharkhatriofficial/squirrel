use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::pmm::PhysicalMemoryManager;
use spin::Mutex;

pub struct VirtualMemoryManager {
    mapper: OffsetPageTable<'static>,
}

impl VirtualMemoryManager {
    /// Initialize using the page table Limine already set up.
    pub fn new(hhdm_offset: u64) -> Self {
        let phys_offset = VirtAddr::new(hhdm_offset);
        let mapper = unsafe {
            // Read CR3 to get current page table root
            let (frame, _) = x86_64::registers::control::Cr3::read();
            let virt = phys_offset + frame.start_address().as_u64();
            let page_table: &mut PageTable = &mut *(virt.as_mut_ptr());
            OffsetPageTable::new(page_table, phys_offset)
        };
        Self { mapper }
    }

    /// Map a virtual page to a physical frame.
    pub fn map_page(
        &mut self,
        virt: VirtAddr,
        phys: PhysFrame,
        flags: PageTableFlags,
        pmm: &Mutex<PhysicalMemoryManager>,
    ) -> Result<(), &'static str> {
        let page: Page<Size4KiB> = Page::containing_address(virt);
        let mut allocator = PmmFrameAllocator(pmm);
        unsafe {
            self.mapper
                .map_to(page, phys, flags, &mut allocator)
                .map_err(|_| "VMM: map_to failed")?
                .flush();
        }
        Ok(())
    }

    /// Translate a virtual address to its physical address.
    pub fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        use x86_64::structures::paging::mapper::TranslateResult;
        use x86_64::structures::paging::Translate;
        match self.mapper.translate(virt) {
            TranslateResult::Mapped { frame, offset, .. } => {
                let phys = match frame {
                    x86_64::structures::paging::mapper::MappedFrame::Size4KiB(f) => {
                        f.start_address()
                    }
                    x86_64::structures::paging::mapper::MappedFrame::Size2MiB(f) => {
                        f.start_address()
                    }
                    x86_64::structures::paging::mapper::MappedFrame::Size1GiB(f) => {
                        f.start_address()
                    }
                };
                Some(phys + offset)
            }
            _ => None,
        }
    }
}

/// Adapter so VMM can ask PMM for frames when creating page table entries
struct PmmFrameAllocator<'a>(&'a Mutex<PhysicalMemoryManager>);

unsafe impl<'a> FrameAllocator<Size4KiB> for PmmFrameAllocator<'a> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        self.0.lock().allocate_frame()
    }
}
