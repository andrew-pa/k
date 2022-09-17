#!/bin/bash
cd kernel
cargo build --verbose --target=aarch64-unknown-none
cd ..
./make-image.sh kernel/target/aarch64-unknown-none/debug/kernel .build/kernel.img
