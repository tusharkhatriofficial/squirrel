use crate::println;
use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;
use spin::Once;

pub mod heap;
pub mod pmm;
pub mod vmm;

use pmm::PhysicalMemoryManager;
use vmm::VirtualMemoryManager;

pub static PMM: Once<spin::Mutex<PhysicalMemoryManager>> = Once::new();
pub static VMM: Once<spin::Mutex<VirtualMemoryManager>> = Once::new();

pub fn init(mmap: &MemoryMapResponse, hhdm_offset: u64) {
    println!("[Memory] Parsing memory map...");

    // Count usable bytes
    let total_usable: u64 = mmap
        .entries()
        .iter()
        .filter(|e| e.entry_type == EntryType::USABLE)
        .map(|e| e.length)
        .sum();
    println!("[Memory] Usable RAM: {} MB", total_usable / 1024 / 1024);

    // Init physical memory manager
    let pmm = PhysicalMemoryManager::new(mmap, hhdm_offset);
    PMM.call_once(|| spin::Mutex::new(pmm));
    println!(
        "[Memory] PMM: {} free frames",
        PMM.get().unwrap().lock().free_frames()
    );

    // Init virtual memory manager (uses Limine's page table as base)
    let vmm = VirtualMemoryManager::new(hhdm_offset);
    VMM.call_once(|| spin::Mutex::new(vmm));
    println!("[Memory] VMM: 4-level paging active");

    // Init heap
    heap::init_heap(VMM.get().unwrap(), PMM.get().unwrap());
    println!(
        "[Memory] Heap: {} MB at {:#x}",
        heap::HEAP_SIZE / 1024 / 1024,
        heap::HEAP_START
    );

    // Sanity test — verify alloc works
    {
        extern crate alloc;
        use alloc::vec::Vec;
        let mut v: Vec<u64> = Vec::new();
        for i in 0..16 {
            v.push(i);
        }
        assert_eq!(v[15], 15, "heap sanity check failed");
    }
    println!("[Memory] Heap self-test: OK");
}
