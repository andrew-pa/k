add-symbol-file target/aarch64-unknown-none/debug/kernel
target remote localhost:1234
break kernel::kmain
# break _start
break *0x41000000
continue
