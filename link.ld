SECTIONS {
    . = 0x44004400;
    .text : { *(.text) }
    .data : { *(.data) }
    .bss : { *(.bss) }
}
