# k

An operating system written in Rust targeting modern ARMv8 64-bit processors.

Right now the system only supports running on the QEMU `virt` board, but should be portable to most reasonable ARMv8 systems that can run U-Boot and have an available device tree.

## Features:
- Debug UART output
- Virtual memory management
- Simple thread scheduler and processes
- GICv2 interrupt controller support with v2m message-based interrupt support
- PCIe driver
- NVMe driver (work in progress)
- FAT file system driver (work in progress)


© 2022-2024 Andrew Palmer
