ENTRY(_start)

SECTIONS {
    /* Higher-half kernel: starts at -2GB in virtual address space */
    . = 0xFFFFFFFF80000000;

    .text : {
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

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
