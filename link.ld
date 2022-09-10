SECTIONS {
    . = 0x0;
    .text : { *(.text) }
    .data : { *(.data) }
    __bss_start = .;
    .bss : { *(.bss) }
}
