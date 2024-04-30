SECTIONS {
    . = 0xffff000041000000 ;
    __kernel_start = . ;
    .text : {
        *(.text.boot)
        *(.text .text.*)
    }
    .data : { *(.data .data.*) }
    .rodata : { *(.rodata .rodata.*) }
    .bss : {
        __bss_start = . ;
        *(.bss .bss.*)
        __bss_end = . ;
    }
    .stack (NOLOAD) : {
        . += 4M ;
        . = ALIGN(16);
        __stack_start = . ;
    }
    __kernel_end = . ;
}
ENTRY(_start)
