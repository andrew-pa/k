#!/bin/bash

boot_dir="$1"
boot_args="$2"

# make $@ everything after $2
shift 2

./qemu/build/qemu-system-aarch64 -machine virt -cpu cortex-a57 -nographic \
    -bios ./u-boot/.build/u-boot.bin -semihosting \
    -drive if=none,file=fat:rw:$boot_dir,id=kboot,format=raw \
    -device nvme,drive=kboot,serial=foo "$@" \
<<-END
    nvme scan
    fatload nvme 0 0x41000000 kernel.img
    env set bootargs '$boot_args'
    bootm 41000000 - 40000000
END

