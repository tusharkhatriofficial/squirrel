use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;
use spin::Once;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

static TSS: Once<TaskStateSegment> = Once::new();
static GDT: Once<(GlobalDescriptorTable, Selectors)> = Once::new();

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    let tss = TSS.call_once(|| {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            // SAFETY: only used once during init, pointer is not dereferenced mutably
            let stack_start = VirtAddr::from_ptr(&raw const STACK as *const u8);
            stack_start + STACK_SIZE as u64
        };
        tss
    });

    let (gdt, selectors) = GDT.call_once(|| {
        let mut gdt = GlobalDescriptorTable::new();
        let code = gdt.append(Descriptor::kernel_code_segment());
        let data = gdt.append(Descriptor::kernel_data_segment());
        let tss = gdt.append(Descriptor::tss_segment(tss));
        (
            gdt,
            Selectors {
                code_selector: code,
                data_selector: data,
                tss_selector: tss,
            },
        )
    });

    gdt.load();
    unsafe {
        CS::set_reg(selectors.code_selector);
        DS::set_reg(selectors.data_selector);
        SS::set_reg(SegmentSelector(0)); // null segment for SS in 64-bit mode
        load_tss(selectors.tss_selector);
    }
}
