
/* start.S */

.section .text.boot

.global _start

_start:
    /* disable MMU */
    mrs x0, SCTLR_EL1
    bic x0, x0, #1
    msr SCTLR_EL1, x0
    isb

    /* set TTLB0/1 to fixed(?) map that sets up the kernel mapping correctly and identity maps the next instructions*/
    adr x0, _kernel_page_table_root
    msr TTBR0_EL1, x0
    msr TTBR1_EL1, x0

    /* set TCR to known correct value*/
    ldr x0, _init_tcr
    msr TCR_EL1, x0

    /* enable the MMU */
    mrs x0, SCTLR_EL1
    orr x0, x0, #1
    msr SCTLR_EL1, x0
    isb

    /* start the kernel, now running in the correct spot in virtual memory */
    b kmain
    .quad 0xbeefbeefbeefbeef

.section .data.boot

.global _kernel_page_table_root

_kernel_page_table_root:
    .quad 0

_init_tcr:
    .quad 0xffff
