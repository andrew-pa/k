use bitfield::bitfield;
use chashmap::{ReadGuard, WriteGuard, CHashMap};
use core::{cell::{RefCell, OnceCell}, ops::Deref};
use hashbrown::HashMap;
use lock_api::{MappedRwLockReadGuard, RwLockReadGuard};
use smallvec::SmallVec;
use spin::lock_api::{Mutex, RwLock};

use crate::{
    exception::Registers,
    memory::{paging::PageTable, PhysicalAddress},
};

pub type CHashMapG<K, V> = CHashMap<K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGReadGuard<'a, K, V> = ReadGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;
pub type CHashMapGWriteGuard<'a, K, V> = WriteGuard<'a, K, V, hashbrown::hash_map::DefaultHashBuilder, spin::RwLock<()>>;

pub type ProcessId = u32;
pub type ThreadId = u32;

pub struct Process {
    id: ProcessId,
    page_tables: PageTable,
    threads: SmallVec<[ThreadId; 4]>,
}

pub struct Thread {
    id: ThreadId,
    /// None => kernel thread
    parent: Option<ProcessId>,
    register_state: Registers,
    program_status: SavedProgramStatus,
    pc: usize,
}

static mut PROCESSES: OnceCell<CHashMapG<ProcessId, Process>> = OnceCell::new();
static mut THREADS: OnceCell<CHashMapG<ThreadId, Thread>> = OnceCell::new();

pub fn processes() -> &'static CHashMapG<ProcessId, Process> {
    unsafe { PROCESSES.get_or_init(Default::default) }
}

pub fn threads() -> &'static CHashMapG<ThreadId, Thread> {
    unsafe { THREADS.get_or_init(Default::default) }
}

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

pub fn read_saved_program_status() -> SavedProgramStatus {
    let mut v: u64;
    unsafe {
        core::arch::asm!("mrs {v}, SPSR_EL1", v = out(reg) v);
    }
    SavedProgramStatus(v)
}

pub fn write_saved_program_status(spsr: SavedProgramStatus) {
    unsafe {
        core::arch::asm!("msr SPSR_EL1, {v}", v = in(reg) spsr.0);
    }
}
