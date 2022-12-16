
/* start.S */

.global _start

_start:
    b kmain
    .quad 0xbeefbeefbeefbeef
