#!/bin/bash
# set -v
cd ..
echo $PWD
img_name=$(echo $1 | shasum | cut -d ' ' -f 1).img
echo $1 "->" $img_name
./make-image.sh $1 .build/$img_name
qemu-system-aarch64 -machine virt -cpu cortex-a57 \
    -display none -monitor none -serial stdio -semihosting \
    -bios ./u-boot/.build/u-boot.bin \
    -drive if=none,file=fat:rw:./.build,id=test,format=raw \
    -device nvme,drive=test,serial=foo \
<<END
    nvme scan
    fatload nvme 0 0x41000000 $img_name
    bootm 41000000
END
