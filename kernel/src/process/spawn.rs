use alloc::format;
use elf::segment::ProgramHeader;
use kapi::queue::{FIRST_RECV_QUEUE_ID, FIRST_SEND_QUEUE_ID};

use crate::error::{self, Error, MemorySnafu};

use super::*;

fn load_segment(
    seg: &ProgramHeader,
    src_data: &[u8],
    pt: &mut PageTable,
    address_space_allocator: &mut VirtualAddressAllocator,
) -> Result<(), Error> {
    log::trace!("mapping segment {seg:x?}");
    let page_aligned_vaddr = VirtualAddress((seg.p_vaddr as usize) & !(PAGE_SIZE - 1));
    let page_alignment_offset = (seg.p_vaddr as usize) & (PAGE_SIZE - 1);
    let page_count = (seg.p_memsz as usize + page_alignment_offset).div_ceil(PAGE_SIZE);
    let mut dest_memory_segment =
        PhysicalBuffer::alloc(page_count, &Default::default()).context(error::MemorySnafu {
            reason: "allocate process memory segement",
        })?;

    log::trace!(
        "\tpage aligned p_vaddr = {page_aligned_vaddr}, alignment_offset = {page_alignment_offset:x}, physical buffer = {:?}, page count = {page_count}",
        dest_memory_segment
    );

    let src_start = seg.p_offset as usize;
    let src_end = src_start + (seg.p_filesz as usize);
    let dest_end = page_alignment_offset + seg.p_filesz as usize;
    log::trace!("copying {src_start}..{src_end} to {page_alignment_offset}..{dest_end}");
    log::trace!(
        "dst={} src={:?}",
        dest_memory_segment.virtual_address(),
        src_data.as_ptr() as *mut u8
    );
    if src_end > src_start {
        dest_memory_segment.as_bytes_mut()[page_alignment_offset..dest_end]
            .copy_from_slice(&src_data[src_start..src_end]);
    }
    if seg.p_memsz > seg.p_filesz {
        log::trace!("zeroing {} bytes", seg.p_memsz - seg.p_filesz);
        dest_memory_segment.as_bytes_mut()[seg.p_filesz as usize..].fill(0);
    }
    // let go of buffer, we will free pages by walking the page table when the process dies
    let (pa, _) = dest_memory_segment.unmap();
    // TODO: set correct page flags beyond R/W, perhaps also parse p_flags more rigorously
    pt.map_range(
        pa,
        page_aligned_vaddr,
        page_count,
        true,
        &PageTableEntryOptions {
            read_only: !seg.p_flags.bit(1),
            el0_access: true,
        },
    )
    .with_context(|_| error::MemorySnafu {
        reason: format!("map process segment {seg:?}"),
    })?;
    address_space_allocator
        .reserve(page_aligned_vaddr, page_count)
        .expect("user process VA allocations should not overlap, and the page table should check");
    Ok(())
}

/// Creates a new process from a binary loaded from a path in the registry.
/// Supports ELF binaries.
pub async fn spawn_process(
    binary_path: impl AsRef<crate::registry::Path>,
    parameter_buffer: Option<(PhysicalBuffer, usize)>,
    before_launch: impl FnOnce(Arc<Process>),
) -> Result<Arc<Process>, Error> {
    use crate::registry::registry;

    // load & parse binary
    let path = binary_path.as_ref();
    log::debug!("spawning process with binary file at {path}");
    let f = registry().open_file(path).await?;

    let f_len = f.len() as usize;
    log::debug!("binary file size = {f_len}");
    let mut src_data = PhysicalBuffer::alloc(f_len.div_ceil(PAGE_SIZE), &Default::default())
        .context(error::MemorySnafu {
            reason: "allocate temporary buffer for binary",
        })?;
    f.read(0, &mut [&mut src_data.as_bytes_mut()[0..f_len]])
        .await?;

    // parse ELF binary
    let bin: elf::ElfBytes<elf::endian::LittleEndian> =
        elf::ElfBytes::minimal_parse(src_data.as_bytes())
            .map_err(|e| Error::other("parse ELF binary", None, e))?;
    // log::debug!("ELF header = {:#x?}", bin.ehdr);

    // allocate a process ID
    let pid = unsafe {
        use core::sync::atomic::Ordering;
        NonZeroU32::new_unchecked(NEXT_PID.fetch_add(1, Ordering::AcqRel))
    };

    // create page tables for the new process
    // TODO: ASID calculation is probably not ideal.
    let mut pt = PageTable::empty(false, pid.get() as u16).context(error::MemorySnafu {
        reason: "create process page tables",
    })?;

    // create an address space allocator for the entire address space of the process
    let mut address_space_allocator =
        VirtualAddressAllocator::new(VirtualAddress(PAGE_SIZE), 0x0000_ffff_ffff_ffff / PAGE_SIZE);

    // load segments from ELF binary into memory
    let segments = bin.segments().context(error::MiscSnafu {
        reason: "expected binary to have at least one segment",
        code: Some(kapi::completions::ErrorCode::BadFormat),
    })?;

    for seg in segments {
        // only consider PT_LOAD=1 segements
        if seg.p_type != 1 {
            continue;
        }
        load_segment(
            &seg,
            src_data.as_bytes(),
            &mut pt,
            &mut address_space_allocator,
        )?;
    }

    let (params_addr, params_size) = if let Some((buf, actual_size)) = parameter_buffer {
        let (phy, page_count) = buf.unmap();
        let addr = address_space_allocator
            .alloc(page_count)
            .context(MemorySnafu {
                reason: "allocate virtual addresses for process parameters",
            })?;
        pt.map_range(
            phy,
            addr,
            page_count,
            true,
            &PageTableEntryOptions {
                read_only: false,
                el0_access: true,
            },
        )
        .context(error::MemorySnafu {
            reason: "map process parameters",
        })?;
        (addr, actual_size)
    } else {
        (VirtualAddress(0), 0)
    };

    // create process structure
    let proc = Arc::new(Process {
        id: pid,
        page_tables: pt,
        threads: Default::default(),
        next_queue_id: AtomicU16::new(FIRST_RECV_QUEUE_ID.into()),
        send_queues: Default::default(),
        recv_queues: Default::default(),
        address_space_allocator: Mutex::new(address_space_allocator),
        exit_state: Default::default(),
        next_handle: AtomicU32::new(1),
        open_files: Default::default(),
    });
    processes().push(proc.clone());

    // create initial queues for process
    const FIRST_QUEUE_SIZE: usize = 64;
    let recv_qu = proc
        .create_completion_queue(&kapi::commands::CreateCompletionQueue {
            size: FIRST_QUEUE_SIZE,
        })
        .await?;
    assert_eq!(recv_qu.id, FIRST_RECV_QUEUE_ID);
    let send_qu = proc
        .create_submission_queue(&kapi::commands::CreateSubmissionQueue {
            size: FIRST_QUEUE_SIZE,
            associated_completion_queue: recv_qu.id,
        })
        .await?;
    assert_eq!(send_qu.id, FIRST_SEND_QUEUE_ID);
    log::trace!("created first queues for process: {send_qu:?}/{recv_qu:?}");

    // create main thread's stack
    // TODO: this should be a parameter
    let stack_page_count = 512;
    let stack_vaddr = proc.alloc_memory(stack_page_count, false).await?.0;

    // create main thread
    let tid = next_thread_id();
    let start_regs = Registers::from_args(&[
        send_qu.start,
        FIRST_QUEUE_SIZE,
        recv_qu.start,
        FIRST_QUEUE_SIZE,
        params_addr.0,
        params_size,
    ]);
    let main_thread = Arc::new(Thread::user_thread(
        proc.clone(),
        tid,
        VirtualAddress(bin.ehdr.e_entry as usize),
        stack_vaddr,
        stack_page_count,
        ThreadPriority::Normal,
        start_regs,
    ));
    proc.threads.lock_blocking().push(main_thread.clone());
    threads().push(main_thread.clone());

    // run any custom code before the process is eligible to be scheduled but after it has been created.
    // this allows the caller to attach resources that requires .awaiting etc.
    before_launch(proc.clone());

    // schedule the process' main thread
    thread::scheduler::scheduler().add_thread(main_thread);

    Ok(proc)
}
