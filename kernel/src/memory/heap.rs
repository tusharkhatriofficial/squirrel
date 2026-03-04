use linked_list_allocator::LockedHeap;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::memory::{pmm::PhysicalMemoryManager, vmm::VirtualMemoryManager};
use spin::Mutex;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub const HEAP_START: usize = 0xFFFF_FFFF_9000_0000;
pub const HEAP_SIZE: usize = 8 * 1024 * 1024; // 8 MB initial heap

pub fn init_heap(vmm: &Mutex<VirtualMemoryManager>, pmm: &Mutex<PhysicalMemoryManager>) {
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

    for offset in (0..HEAP_SIZE).step_by(4096) {
        let virt = VirtAddr::new((HEAP_START + offset) as u64);
        let frame = pmm
            .lock()
            .allocate_frame()
            .expect("OOM: cannot allocate heap");
        vmm.lock()
            .map_page(virt, frame, flags, pmm)
            .expect("VMM: heap mapping failed");
    }

    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
}
