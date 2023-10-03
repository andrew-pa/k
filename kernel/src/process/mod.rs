use crate::{
    exception::Registers,
    memory::{paging::PageTable, PhysicalBuffer, VirtualAddress, PAGE_SIZE},
    CHashMapG,
};
use bitfield::bitfield;
use core::{cell::OnceCell, error::Error, sync::atomic::AtomicU32};
use smallvec::{smallvec, SmallVec};
use snafu::{ensure, OptionExt, ResultExt, Snafu};

pub mod scheduler;

pub type ProcessId = u32;
pub type ThreadId = u32;

pub struct Process {
    pub id: ProcessId,
    pub page_tables: PageTable,
    pub threads: SmallVec<[ThreadId; 4]>,
}

#[derive(Copy, Clone, Debug)]
pub enum ThreadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

pub struct Thread {
    pub id: ThreadId,
    /// None => kernel thread
    pub parent: Option<ProcessId>,
    pub register_state: Registers,
    pub program_status: SavedProgramStatus,
    pub pc: VirtualAddress,
    pub sp: VirtualAddress,
    pub priority: ThreadPriority,
}

impl Thread {
    /// Save the thread that was interrupted by an exception into this thread
    /// by copying the SPSR and ELR registers
    pub unsafe fn save(&mut self, regs: &Registers) {
        self.register_state = *regs;
        self.program_status = read_saved_program_status();
        self.pc = read_exception_link_reg();
        self.sp = read_stack_pointer(0);
    }

    /// Restore this thread so that it will resume when the kernel finishes processesing an exception
    pub unsafe fn restore(&self, regs: &mut Registers) {
        write_exception_link_reg(self.pc);
        write_stack_pointer(0, self.sp);
        write_saved_program_status(&self.program_status);
        *regs = self.register_state;
    }

    pub fn kernel_thread(id: ThreadId, start: fn() -> !, stack: &PhysicalBuffer) -> Self {
        Thread {
            id,
            parent: None,
            register_state: Registers::default(),
            program_status: SavedProgramStatus::initial_for_el1(),
            pc: (start as *const ()).into(),
            sp: stack.virtual_address().offset((stack.len() - 16) as isize),
            priority: ThreadPriority::Normal,
        }
    }

    pub fn user_thread(
        pid: ProcessId,
        tid: ThreadId,
        pc: VirtualAddress,
        sp: VirtualAddress,
        priority: ThreadPriority,
    ) -> Self {
        Thread {
            id: tid,
            parent: Some(pid),
            register_state: Registers::default(),
            program_status: SavedProgramStatus::initial_for_el0(),
            pc,
            sp,
            priority,
        }
    }
}

/// the idle thread is dedicated to handling interrupts, i.e. it is the thread holding the EL1 stack
pub const IDLE_THREAD: ThreadId = 0;
/// the task thread runs the async task executor on its own stack at SP_EL0
pub const TASK_THREAD: ThreadId = 1;

// TODO: what we really want here is a concurrent SlotMap
static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();
static mut THREADS: OnceCell<CHashMapG<ThreadId, Thread>> = OnceCell::new();

static mut NEXT_PID: AtomicU32 = AtomicU32::new(1);
static mut NEXT_TID: AtomicU32 = AtomicU32::new(TASK_THREAD + 1);

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

pub fn threads() -> &'static CHashMapG<ThreadId, Thread> {
    unsafe {
        THREADS.get_or_init(|| {
            let mut ths: CHashMapG<ThreadId, Thread> = Default::default();
            // Create the idle thread, which will just wait for interrupts
            let mut program_status = SavedProgramStatus::initial_for_el1();
            program_status.set_sp(true); // the idle thread runs on the EL1 stack normally used by interrupts and kmain
            ths.insert(
                IDLE_THREAD,
                Thread {
                    id: IDLE_THREAD,
                    parent: None,
                    register_state: Registers::default(),
                    program_status,
                    pc: VirtualAddress(0),
                    sp: VirtualAddress(0),
                    priority: ThreadPriority::Low,
                },
            );
            ths
        })
    }
}

pub fn next_thread_id() -> ThreadId {
    use core::sync::atomic::Ordering;
    unsafe { NEXT_TID.fetch_add(1, Ordering::AcqRel) }
}

pub fn spawn_thread(thread: Thread) {
    let id = thread.id;
    threads().insert(id, thread);
    scheduler::scheduler().add_thread(id);
}

#[derive(Debug, Snafu)]
pub enum SpawnError {
    Registry {
        path: crate::registry::PathBuf,
        source: crate::registry::RegistryError,
    },
    Memory {
        reason: &'static str,
        source: crate::memory::MemoryError,
    },
    MemoryMap {
        source: crate::memory::paging::MapError,
    },
    FileSystem {
        reason: &'static str,
        source: crate::fs::Error,
    },
    BinFormat {
        reason: elf::ParseError,
    },
    Other {
        reason: &'static str,
    },
}

pub async fn spawn_process(
    binary_path: impl AsRef<crate::registry::Path>,
) -> Result<ProcessId, SpawnError> {
    use crate::registry::registry;
    use alloc::vec::Vec;

    // load & parse binary
    let path = binary_path.as_ref();
    log::debug!("spawning process with binary file at {path}");
    let mut f = registry()
        .open_byte_store(path)
        .await
        .with_context(|_| RegistrySnafu { path })?;

    // we will directly map portions of the buffer into the new process's address space, so we directly use physical memory.
    let f_len = f.len() as usize;
    log::debug!("binary file size = {f_len}");
    let mut data = alloc::vec![0u8; f_len];

    // actually read the file
    let mut i = 0;
    while i < f_len {
        log::trace!("reading binary, offset = {i:x}");
        let nb = f.read(&mut data[i..]).await.context(FileSystemSnafu {
            reason: "read binary file",
        })?;
        if nb == 0 {
            break;
        }
        i += nb;
    }
    ensure!(
        i == f_len,
        OtherSnafu {
            reason: "file read to unexpected size"
        }
    );

    // parse ELF binary
    let bin: elf::ElfBytes<elf::endian::AnyEndian> =
        elf::ElfBytes::minimal_parse(&data).map_err(|reason| BinFormatSnafu { reason }.build())?;
    log::debug!("ELF header = {:#x?}", bin.ehdr);

    // allocate a process ID
    let pid = unsafe {
        use core::sync::atomic::Ordering;
        NEXT_PID.fetch_add(1, Ordering::AcqRel)
    };

    // create page tables for the new process
    // TODO: ASID calculation is probably not ideal.
    let mut pt = PageTable::empty(false, pid as u16).context(MemorySnafu {
        reason: "create process page tables",
    })?;
    for seg in bin.segments().context(OtherSnafu {
        reason: "expected binary to have at least one segment",
    })? {
        log::debug!("have segment {seg:x?}");
    }
    for seg in bin.segments().context(OtherSnafu {
        reason: "expected binary to have at least one segment",
    })? {
        // only consider PT_LOAD=1 segements
        if seg.p_type != 1 {
            continue;
        }
        log::trace!("mapping segment {seg:x?}");
        // sometimes the segment p_vaddr is unaligned. we need to map an aligned version and then
        // offset the copy into the buffer. TODO: we might need to allocate more memory to account
        // for the offset.
        let aligned_vaddr = VirtualAddress((seg.p_vaddr & !(seg.p_align - 1)) as usize);
        let alignment_offset = (seg.p_vaddr & (seg.p_align - 1)) as usize;
        log::trace!(
            "segment aligned p_vaddr = {aligned_vaddr}, alignment_offset = {alignment_offset:x}"
        );
        let mut buf = PhysicalBuffer::alloc(
            (seg.p_memsz as usize).div_ceil(PAGE_SIZE),
            &Default::default(),
        )
        .context(MemorySnafu {
            reason: "allocate process memory segement",
        })?;
        let start = seg.p_offset as usize;
        let end = start + (seg.p_filesz as usize);
        log::trace!("copying {start}..{end}");
        if end > start {
            (&mut buf.as_bytes_mut()[alignment_offset..(alignment_offset + seg.p_filesz as usize)])
                .copy_from_slice(&data[start..end]);
        }
        if seg.p_memsz > seg.p_filesz {
            log::trace!("zeroing {} bytes", seg.p_memsz - seg.p_filesz);
            (&mut buf.as_bytes_mut()[seg.p_filesz as usize..]).fill(0);
        }
        // let go of buffer, we will free pages by walking the page table when the process dies
        let (pa, _) = buf.unmap();
        log::trace!("segment phys addr = {pa}");
        // TODO: set correct page flags
        pt.map_range(
            pa,
            aligned_vaddr,
            (seg.p_memsz as usize).div_ceil(PAGE_SIZE),
            true,
            &Default::default(),
        )
        .context(MemoryMapSnafu)?;
    }

    let tid = next_thread_id();

    // create process structure
    let proc = Process {
        id: pid,
        page_tables: pt,
        threads: smallvec![tid],
    };
    let p = processes().insert(pid, proc);
    assert!(p.is_none());

    // create thread 0 for process
    spawn_thread(Thread::user_thread(
        pid,
        tid,
        VirtualAddress(todo!("entry")),
        VirtualAddress(todo!("stack")),
        ThreadPriority::Normal,
    ));

    Ok(pid)
}

// should this really be here?
bitfield! {
    pub struct SavedProgramStatus(u64);
    impl Debug;
    n, set_n: 31;
    z, set_z: 30;
    c, set_c: 29;
    v, set_v: 28;

    tco, set_tco: 25;
    dit, set_dit: 24;
    uao, set_uao: 23;
    pan, set_pan: 22;
    ss, set_ss: 21;
    il, set_il: 20;

    allint, set_allint: 13;
    ssbs, set_ssbs: 12;
    btype, set_btype: 11, 10;

    d, set_d: 9;
    a, set_a: 8;
    i, set_i: 7;
    f, set_f: 6;

    el, set_el: 3, 2;

    sp, set_sp: 0;
}

impl SavedProgramStatus {
    /// This creates a suitable SPSR value for a thread running at EL0 (using the SP_EL0 stack pointer)
    pub fn initial_for_el0() -> SavedProgramStatus {
        SavedProgramStatus(0)
    }

    /// This creates a suitable SPSR value for a thread running at EL1 with its own stack using the
    /// SP_EL0 stack pointer
    pub fn initial_for_el1() -> SavedProgramStatus {
        let mut spsr = SavedProgramStatus(0);
        spsr.set_el(1);
        spsr
    }
}

pub fn read_saved_program_status() -> SavedProgramStatus {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, SPSR_EL1", v = out(reg) v);
    }
    SavedProgramStatus(v)
}

pub fn write_saved_program_status(spsr: &SavedProgramStatus) {
    unsafe {
        core::arch::asm!("msr SPSR_EL1, {v}", v = in(reg) spsr.0);
    }
}

/// Read the value of the program counter when the exception occured
pub fn read_exception_link_reg() -> VirtualAddress {
    let mut v: usize;
    unsafe {
        core::arch::asm!("mrs {v}, ELR_EL1", v = out(reg) v);
    }
    VirtualAddress(v)
}

/// Write the value that the program counter will assume when the exception handler is finished
pub fn write_exception_link_reg(addr: VirtualAddress) {
    unsafe {
        core::arch::asm!("msr ELR_EL1, {v}", v = in(reg) addr.0);
    }
}

pub fn read_stack_pointer(el: u8) -> VirtualAddress {
    let mut v: usize;
    unsafe {
        match el {
            0 => core::arch::asm!("mrs {v}, SP_EL0", v = out(reg) v),
            1 => core::arch::asm!("mrs {v}, SP_EL1", v = out(reg) v),
            2 => core::arch::asm!("mrs {v}, SP_EL2", v = out(reg) v),
            // 3 => core::arch::asm!("mrs {v}, SP_EL3", v = out(reg) v),
            _ => panic!("invalid exception level {el}"),
        }
    }
    VirtualAddress(v)
}

pub unsafe fn write_stack_pointer(el: u8, sp: VirtualAddress) {
    match el {
        0 => core::arch::asm!("msr SP_EL0, {v}", v = in(reg) sp.0),
        1 => core::arch::asm!("msr SP_EL1, {v}", v = in(reg) sp.0),
        2 => core::arch::asm!("msr SP_EL2, {v}", v = in(reg) sp.0),
        // 3 => core::arch::asm!("msr SP_EL3, {v}", v = in(reg) sp.0),
        _ => panic!("invalid exception level {el}"),
    }
}
