# k

An operating system kernel written in Rust targeting modern ARMv8 64-bit processors.

Right now the system only supports running on the QEMU `virt` board, but should be portable to most reasonable ARMv8 systems that can run U-Boot and have an available device tree at boot.

## Features:
- Debug UART output
- Virtual memory management
- Simple thread scheduler
- GICv2 interrupt controller support with v2m message-based interrupt support
- PCIe driver (work in progress)
- NVMe driver (work in progress)

## Boot screenshot
![output from serial console on boot](https://user-images.githubusercontent.com/6148347/222933486-2cfc9ec1-020c-4a4f-9d60-f7fae402e97b.png)



© 2022-2023 Andrew Palmer
