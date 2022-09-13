#!/bin/bash
qemu-system-aarch64 -machine virt -cpu cortex-a57 -nographic \
    -bios ./u-boot/.build/u-boot.bin \
    -drive if=none,file=fat:rw:./.build,id=test,format=raw \
    -device nvme,drive=test,serial=foo \
<<-END
    nvme scan
    fatload nvme 0 0x44004400 kernel.img
    bootm 44004400
END

