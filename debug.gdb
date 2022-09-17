add-symbol-file kernel/target/aarch64-unknown-none/debug/kernel
target remote localhost:1234
break kernel::kmain
continue
