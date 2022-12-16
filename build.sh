#!/bin/bash
set -e
# this is sus
export CC_aarch64_unknown_none=aarch64-linux-gnu-gcc
cd kernel
cargo build --verbose --target=aarch64-unknown-none
cd ..
./make-image.sh kernel/target/aarch64-unknown-none/debug/kernel .build/kernel.img
