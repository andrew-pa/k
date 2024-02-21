#!/bin/bash
./qemu/.build/qemu-system-aarch64 -machine virt -cpu cortex-a57 -nographic \
    -bios ./u-boot/.build/u-boot.bin -semihosting \
    -drive if=none,file=fat:rw:$1,id=kboot,format=raw \
    -device nvme,drive=kboot,serial=foo $3 \
<<-END
    nvme scan
    fatload nvme 0 0x41000000 kernel.img
    env set bootargs '$2'
    bootm 41000000 - 40000000
END

