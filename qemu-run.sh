#!/bin/bash
./qemu/.build/qemu-system-aarch64 -machine virt -cpu cortex-a57 -nographic \
    -bios ./u-boot/.build/u-boot.bin -semihosting \
    -drive if=none,file=fat:rw:./.build,id=test,format=raw \
    -device nvme,drive=test,serial=foo \
<<-END
    nvme scan
    fatload nvme 0 0x41000000 kernel.img
    bootm 41000000
END

