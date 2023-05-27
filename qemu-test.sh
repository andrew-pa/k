#!/bin/bash
# set -v
scratch_disk=/tmp/k_scratch_disk.qcow2
cd ..
echo $PWD
if [ ! -f $scratch_disk ]; then
    qemu-img create -f qcow2 $scratch_disk 1M
fi
img_name=$(echo $1 | shasum | cut -d ' ' -f 1).img
echo $1 "->" $img_name
./make-image.sh $1 .build/$img_name
qemu-system-aarch64 -machine virt -cpu cortex-a57 \
    -display none -monitor none -serial stdio -semihosting \
    -bios ./u-boot/.build/u-boot.bin \
    -drive if=none,file=fat:rw:./.build,id=boot,format=raw \
    -device nvme,drive=boot,serial=foo \
    -drive if=none,file=$scratch_disk,id=scratch \
    -device nvme,drive=scratch,serial=bar \
<<END
    nvme scan
    fatload nvme 0 0x41000000 $img_name
    bootm 41000000
END
