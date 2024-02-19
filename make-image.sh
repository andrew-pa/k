#!/bin/bash
# ELF -> U-boot image

GPREFIX=aarch64-linux-gnu-

if ! command -v ${GPREFIX}objcopy > /dev/null; then
    GPREFIX=aarch64-elf-
    if ! command -v ${GPREFIX}objcopy > /dev/null; then
        echo "could not find ${GPREFIX}objcopy"
        exit 1
    fi
fi

# BIN_NAME=/tmp/$(uuidgen).bin
BIN_NAME=./.build/k.img
LOAD_ADDR=41000000

echo "input elf  = ${1}"
echo "output img = ${2}"
echo "binary     = ${BIN_NAME}"

# ENTRY_ADDR=$(${GPREFIX}objdump -f $1 | awk "/start/ { printf(\"%x\", 0x$LOAD_ADDR + \$3) }")
ENTRY_ADDR=$LOAD_ADDR

echo "entry point @ ${ENTRY_ADDR}"

${GPREFIX}objcopy -O binary $1 $BIN_NAME

MKIMAGE=./u-boot/.build/tools/mkimage
if command -v mkimage > /dev/null; then
    MKIMAGE=mkimage
fi


$MKIMAGE -A arm64 -O linux -T kernel -C none -a $LOAD_ADDR \
    -e $ENTRY_ADDR -n "k" -d $BIN_NAME $2


