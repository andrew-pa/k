
/* start.S */

.section .text.boot

.global _start

_start:
    /* disable MMU */
    mrs x0, SCTLR_EL1
    bic x0, x0, #1
    msr SCTLR_EL1, x0
    isb

    adr x0, _kernel_page_table_root

    /* create correct level 0 entry to point to our level 1 table */
    adr x1, _kernel_id_map_level1_table
    /* lsr x1, x1, #12 */
    bic x1, x1, #0xfff
    orr x1, x1, #3
    str x1, [x0]

    /* set TTLB0/1 to fixed(?) map that sets up the kernel mapping correctly and identity maps the next instructions*/
    msr TTBR0_EL1, x0
    msr TTBR1_EL1, x0

    /* set TCR to known correct value 0x0784'8410'3510*/
    mov x0, #0x3510
    movk x0, #0x8410, lsl 16
    movk x0, #0x0784, lsl 32
    msr TCR_EL1, x0

    /* enable the MMU */
    mrs x0, SCTLR_EL1
    orr x0, x0, #1
    msr SCTLR_EL1, x0
    isb

    /* TODO: fix stack pointer */
    movz x0, #0xffff, lsl 48
    add sp, sp, x0

    /* start the kernel, now running in the correct spot in virtual memory */
    adr x0, kmain
    movk x0, #0xffff, lsl 48
    br x0
    .quad 0xbeefbeefbeefbeef

.section .data.boot

.global _kernel_page_table_root

.balign 4096
_kernel_page_table_root:
    .quad 0
    .ds.d 511

.balign 4096
_kernel_id_map_level1_table:
    .quad (0x00000000) | (1 << 10) | 1
    .quad (0x40000000) | (1 << 10) | 1
    .quad (0x80000000) | (1 << 10) | 1
    .ds.d 509
