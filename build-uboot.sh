#!/bin/bash
set -v -e

cd ./u-boot
export CROSS_COMPILE=aarch64-linux-gnu-
mkdir -p ./.build
make O=./.build qemu_arm64_defconfig
make O=./.build -j32 all


