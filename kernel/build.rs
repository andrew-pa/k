fn main() {
    // use the kernel linker script
    println!("cargo:rustc-link-arg=-T./kernel/link.ld");
}
