#!/bin/bash
set -e
# this is sus
export CC_aarch64_unknown_none=aarch64-linux-gnu-gcc
cargo build --verbose --target=aarch64-unknown-none
./make-image.sh target/aarch64-unknown-none/debug/kernel .build/kernel.img
cp target/aarch64-unknown-none/debug/init .build/init
