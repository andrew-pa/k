SECTIONS {
    /* . = 0x44004400; */
    . = 0xffff000000000000 ;
    __kernel_start = . ;
    .text : {
        *(.text._start)
        *(.text .text.*)
    }
    .data : { *(.data .data.*) }
    .rodata : { *(.rodata .rodata.*) }
    .bss : {
        __bss_start = . ;
        *(.bss .bss.*)
        __bss_end = . ;
    }
    __kernel_end = . ;
}
ENTRY(_start)
