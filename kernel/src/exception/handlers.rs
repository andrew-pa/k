//! The actual raw exception handler entry points.

use super::*;

// assembly definition of the exception vector table and the low level code that installs the table
// and the low level handlers that calls into the Rust code.
global_asm!(include_str!("table.S"));

extern "C" {
    /// Install the kernel's exception vector table so the kernel can handle exceptions.
    ///
    /// This function should only be called once at initialization, ideally as early as possible to
    /// catch kernel runtime errors.
    ///
    /// # Safety
    /// This function should be safe as long as `table.S` is correct.
    pub fn install_exception_vector_table();
}

fn handle_system_call(proc: Arc<Process>, thread: Arc<Thread>, regs: &mut Registers, id: u16) {
    match system_call_handlers().get_blocking(&id) {
        Some(h) => (*h)(id, proc, thread, regs),
        None => {
            log::warn!(
                "unknown system call: pid={:?}, tid={}, id = 0x{:x}, registers = {:?}",
                proc.id,
                thread.id,
                id,
                regs
            )
        }
    }
}

#[no_mangle]
unsafe extern "C" fn handle_synchronous_exception(regs: *mut Registers, esr: usize, far: usize) {
    let esr = ExceptionSyndromeRegister(esr as u64);
    let ec = ExceptionClass(esr.ec());

    if !ec.is_system_call()
        && !ec.is_user_space_data_page_fault()
        && !ec.is_user_space_code_page_fault()
    {
        panic!(
            "synchronous exception! {}, FAR={far:x}, registers = {:?}, ELR={:x?}",
            esr,
            regs.as_ref(),
            read_exception_link_reg(),
        );
    }

    let regs = regs
        .as_mut()
        .expect("registers ptr should always be non-null");

    let current_thread = process::scheduler().pause_current_thread(regs);

    let parent = current_thread.parent.clone();
    let previous_asid = parent.as_ref().map(|p| p.asid());

    if let Some(p) = parent.as_ref() {
        p.dispatch_new_commands();
    }

    if ec.is_system_call() {
        // system call
        handle_system_call(
            parent.expect("only user space threads make system calls"),
            current_thread.clone(),
            regs,
            esr.iss() as u16,
        );
    } else if ec.is_user_space_data_page_fault() || ec.is_user_space_code_page_fault() {
        // page fault in user space
        let proc = parent.expect("this exception is only for page faults in a lower exception level and since the kernel runs at EL1, that means that we can only get here from a fault in EL0, therefore there must be a current process running (or something is very wrong)");

        // pause the thread so it won't get scheduled before we fix up its address space
        current_thread.set_state(process::thread::ThreadState::Waiting);

        log::trace!(
            "user space page fault at {far:x}, pid={}, tid={}",
            proc.id,
            current_thread.id
        );

        crate::tasks::spawn(async move {
            proc.on_page_fault(current_thread, VirtualAddress(far))
                .await;
        });

        // schedule the task executor next to reduce latency between processing the page fault and
        // resuming the offending process by hopefully immediately running the above task.
        process::scheduler().make_task_executor_current();
    } else {
        unreachable!()
    }

    process::scheduler().resume_current_thread(regs, previous_asid);
}

#[no_mangle]
unsafe extern "C" fn handle_interrupt(regs: *mut Registers, _esr: usize, _far: usize) {
    let ic = interrupt_controller();

    let regs = regs
        .as_mut()
        .expect("registers ptr should always be non-null");

    let current_thread = process::scheduler().pause_current_thread(regs);
    let parent = current_thread.parent.as_ref();

    while let Some(id) = ic.ack_interrupt() {
        log::trace!("handling interrupt {id} ELR={}", read_exception_link_reg());
        match interrupt_handlers().get_mut_blocking(&id) {
            Some(mut h) => (*h)(id, regs),
            None => log::warn!("unhandled interrupt {id}"),
        }
        log::trace!("finished interrupt {id}");
        ic.finish_interrupt(id);
    }

    if let Some(p) = parent.as_ref() {
        p.dispatch_new_commands();
    }

    process::scheduler().resume_current_thread(regs, parent.map(|p| p.asid()));
}

#[no_mangle]
unsafe extern "C" fn handle_fast_interrupt(_regs: *mut Registers, _esr: usize, _far: usize) {
    let ic = interrupt_controller();
    let id = ic.ack_interrupt().unwrap();
    log::warn!("unhandled fast interrupt {id}");
    ic.finish_interrupt(id);
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
        "unimplemented exception! {}, FAR={far:x}, registers = {:?}",
        ExceptionSyndromeRegister(esr as u64),
        regs.as_ref()
    );
}
