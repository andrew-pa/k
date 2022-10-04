SECTIONS {
    . = 0x44004400;
    .text : { *(.text .text.*) }
    .data : { *(.data .data.*) }
    .rodata : { *(.rodata .rodata.*) }
    .bss : {
        __bss_start = . ;
        *(.bss .bss.*)
        __bss_end = . ;
    }
    __kernel_end = . ;
}
ENTRY(kmain)
