ENTRY(_start)

SECTIONS {
    /* Higher-half kernel: starts at -2GB in virtual address space */
    . = 0xFFFFFFFF80000000;

    .text : {
        *(.text .text.*)
    }

    /* Each section must start on a new 4K page so Limine can apply
       distinct permissions (RX for .text, R for .rodata, RW for .data/.bss) */
    . = ALIGN(4096);

    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(4096);

    /* Limine v8+ scans this section to find protocol request structs.
       Must be writable (Limine writes responses into them). */
    .limine_requests : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    }

    .data : {
        *(.got .got.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.data .data.*)
    }

    . = ALIGN(4096);

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    }

    /* Strip debug info from release builds */
    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame)
        *(.eh_frame_hdr)
    }
}
