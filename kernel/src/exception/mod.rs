use core::arch::global_asm;

global_asm!(include_str!("table.S"));

#[derive(Debug)]
pub struct Registers {
    pub x: [usize; 30],
}

#[no_mangle]
unsafe extern "C" fn handle_synchronous_exception(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "synchronous exception! ESR={esr:x}, FAR={far:x}, registers = {:?}",
        regs.as_ref()
    );
}

#[no_mangle]
unsafe extern "C" fn handle_interrupt(regs: *mut Registers, esr: usize, far: usize) {
    todo!("interrupt, ESR={esr:x}");
}

#[no_mangle]
unsafe extern "C" fn handle_fast_interrupt(regs: *mut Registers, esr: usize, far: usize) {
    todo!("fast interrupt, ESR={esr:x}");
}

#[no_mangle]
unsafe extern "C" fn handle_system_error(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "system error! ESR={esr:x}, FAR={far:x}, registers = {:?}",
        regs.as_ref()
    );
}

#[no_mangle]
unsafe extern "C" fn handle_unimplemented_exception(regs: *mut Registers, esr: usize, far: usize) {
    panic!(
        "unimplemented exception! ESR={esr:x}, FAR={far:x}, registers = {:?}",
        regs.as_ref()
    );
}

extern "C" {
    pub fn install_exception_vector_table();
}
