/* exception table */

.global _exception_vector_table
.global install_exception_vector_table

.macro save_regs
    stp x0, x1, [sp, #0*16]
    stp x2, x3, [sp, #1*16]
    stp x4, x5, [sp, #2*16]
    stp x6, x7, [sp, #3*16]
    stp x8, x9, [sp, #4*16]
    stp x10, x11, [sp, #5*16]
    stp x12, x13, [sp, #6*16]
    stp x14, x15, [sp, #7*16]
    stp x16, x17, [sp, #8*16]
    stp x18, x19, [sp, #9*16]
    stp x20, x21, [sp, #10*16]
    stp x22, x23, [sp, #11*16]
    stp x24, x25, [sp, #12*16]
    stp x26, x27, [sp, #13*16]
    stp x28, x29, [sp, #14*16]
    str x30, [sp, #15*16]
.endm

.macro restore_regs
    ldp x0, x1, [sp, #0*16]
    ldp x2, x3, [sp, #1*16]
    ldp x4, x5, [sp, #2*16]
    ldp x6, x7, [sp, #3*16]
    ldp x8, x9, [sp, #4*16]
    ldp x10, x11, [sp, #5*16]
    ldp x12, x13, [sp, #6*16]
    ldp x14, x15, [sp, #7*16]
    ldp x16, x17, [sp, #8*16]
    ldp x18, x19, [sp, #9*16]
    ldp x20, x21, [sp, #10*16]
    ldp x22, x23, [sp, #11*16]
    ldp x24, x25, [sp, #12*16]
    ldp x26, x27, [sp, #13*16]
    ldp x28, x29, [sp, #14*16]
    ldr x30, [sp, #15*16]
.endm

.macro exception_handler fn_to_call
    sub sp, sp, #8*30
    save_regs

    mov x0, sp
    mrs x1, ESR_EL1
    mrs x2, FAR_EL1
    bl \fn_to_call

    restore_regs
    add sp, sp, #8*30
    eret
.endm

_handle_synchronous:
    exception_handler handle_synchronous_exception

_handle_interrupt:
    exception_handler handle_interrupt

_handle_fast_interrupt:
    exception_handler handle_fast_interrupt

_handle_system_error:
    exception_handler handle_system_error

_handle_unimplemented:
    exception_handler handle_unimplemented_exception

.balign 0x800
_exception_vector_table:
/* current EL with SP0 */
.balign 0x80
    b _handle_synchronous
.balign 0x80
    b _handle_interrupt
.balign 0x80
    b _handle_fast_interrupt
.balign 0x80
    b _handle_system_error
/* current EL with SPX */
.balign 0x80
    b _handle_synchronous
.balign 0x80
    b _handle_interrupt
.balign 0x80
    b _handle_fast_interrupt
.balign 0x80
    b _handle_system_error
/* lower EL using AArch64 */
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented
/* lower EL using AArch32 */
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented
.balign 0x80
    b _handle_unimplemented

install_exception_vector_table:
    adr x0, _exception_vector_table
    msr VBAR_EL1, x0
    ret
