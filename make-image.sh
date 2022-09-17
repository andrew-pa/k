#!/bin/bash
# ELF -> U-boot image

GPREFIX=aarch64-linux-gnu-
# BIN_NAME=/tmp/$(uuidgen).bin
BIN_NAME=./.build/k.img
LOAD_ADDR=44004400

echo "input elf  = ${1}"
echo "output img = ${2}"
echo "binary     = ${BIN_NAME}"

ENTRY_ADDR=$(${GPREFIX}objdump -f $1 | awk "/start/ { print \$3 }")

echo "entry point @ ${ENTRY_ADDR}"

${GPREFIX}objcopy -O binary $1 $BIN_NAME

./u-boot/.build/tools/mkimage -A arm64 -O plan9 -T kernel -C none -a $LOAD_ADDR \
    -e $ENTRY_ADDR -n "k" -d $BIN_NAME $2


