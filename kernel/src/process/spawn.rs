use alloc::format;
use elf::segment::ProgramHeader;
use kapi::queue::{FIRST_RECV_QUEUE_ID, FIRST_SEND_QUEUE_ID};
use spin::once::Once;

use crate::error::{self, Error};

use super::*;

fn load_segment(
    seg: &ProgramHeader,
    src_data: &[u8],
    pt: &mut PageTable,
    address_space_allocator: &mut VirtualAddressAllocator,
) -> Result<(), Error> {
    // TODO: we are doing this wrong. the init ELF file seems to make the alignment_offset end up
    // being the running total size of all segments so far, causing each new segment to have to
    // allocate padding space the size of all previous segments. This is obviously unnecessary.

    log::trace!("mapping segment {seg:x?}");
    let page_aligned_vaddr = VirtualAddress((seg.p_vaddr as usize) & !(PAGE_SIZE - 1));
    let page_alignment_offset = (seg.p_vaddr as usize) & (PAGE_SIZE - 1);
    let page_count = (seg.p_memsz as usize + page_alignment_offset).div_ceil(PAGE_SIZE);
    let mut dest_memory_segment =
        PhysicalBuffer::alloc(page_count, &Default::default()).context(error::MemorySnafu {
            reason: "allocate process memory segement",
        })?;

    log::trace!(
        "\tpage aligned p_vaddr = {page_aligned_vaddr}, alignment_offset = {page_alignment_offset:x}, physical address = {}, page count = {page_count}",
        dest_memory_segment.physical_address()
    );

    let src_start = seg.p_offset as usize;
    let src_end = src_start + (seg.p_filesz as usize);
    let dest_end = page_alignment_offset + seg.p_filesz as usize;
    log::trace!("copying {src_start}..{src_end} to {page_alignment_offset}..{dest_end}");
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

/// Make a channel available to the process via mapped memory.
/// The submission and completion queues will be layed out consecutively in memory, and mapping will be created immediately.
/// Returns the base address and the total length in bytes of the mapped region for the submission and completion queues, respectively.
pub fn attach_queues(
    send_qu: &OwnedQueue<Command>,
    recv_qu: &OwnedQueue<Completion>,
    page_tables: &mut PageTable,
    address_space_allocator: &mut VirtualAddressAllocator,
) -> Result<(VirtualAddress, usize, VirtualAddress, usize), Error> {
    let total_len = send_qu.buffer.len() + recv_qu.buffer.len();
    let num_pages = total_len.div_ceil(PAGE_SIZE);
    let send_base_addr = address_space_allocator
        .alloc(num_pages)
        .context(error::MemorySnafu {
            reason: "allocate in process address space for channel",
        })?;
    let recv_base_addr = send_base_addr.add(send_qu.buffer.len());
    let map_opts = PageTableEntryOptions {
        read_only: false,
        el0_access: true,
    };
    page_tables
        .map_range(
            send_qu.buffer.physical_address(),
            send_base_addr,
            send_qu.buffer.page_count(),
            true,
            &map_opts,
        )
        .context(error::MemorySnafu {
            reason: "map process send queue",
        })?;
    page_tables
        .map_range(
            recv_qu.buffer.physical_address(),
            recv_base_addr,
            recv_qu.buffer.page_count(),
            true,
            &map_opts,
        )
        .context(error::MemorySnafu {
            reason: "map process recv queue",
        })?;
    Ok((
        send_base_addr,
        send_qu.buffer.len(),
        recv_base_addr,
        recv_qu.buffer.len(),
    ))
}

/// Creates a new process from a binary loaded from a path in the registry.
/// Supports ELF binaries.
pub async fn spawn_process(
    binary_path: impl AsRef<crate::registry::Path>,
    before_launch: Option<impl FnOnce(Arc<Process>)>,
) -> Result<Arc<Process>, Error> {
    use crate::registry::registry;

    // load & parse binary
    let path = binary_path.as_ref();
    log::debug!("spawning process with binary file at {path}");
    let mut f = registry().open_file(path).await?;

    let f_len = f.len() as usize;
    log::debug!("binary file size = {f_len}");
    let src_data = PhysicalBuffer::alloc(f_len.div_ceil(PAGE_SIZE), &Default::default()).context(
        error::MemorySnafu {
            reason: "allocate temporary buffer for binary",
        },
    )?;
    f.load_pages(0, src_data.physical_address(), src_data.page_count())
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

    let segments = bin.segments().context(error::ExpectedValueSnafu {
        reason: "expected binary to have at least one segment",
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

    // create process stack
    // TODO: this should be a parameter
    let stack_page_count = 512;
    let stack_vaddr = VirtualAddress(0x0000_ffff_0000_0000);
    let stack_buf = {
        physical_memory_allocator()
            .alloc_contig(stack_page_count)
            .context(error::MemorySnafu {
                reason: "allocate process stack segment",
            })?
    };
    log::trace!("stack buffer @ {stack_buf}");
    pt.map_range(
        stack_buf,
        stack_vaddr,
        stack_page_count,
        true,
        &PageTableEntryOptions {
            read_only: false,
            el0_access: true,
        },
    )
    .context(error::MemorySnafu {
        reason: "map proces stack",
    })?;
    address_space_allocator
        .reserve(stack_vaddr, stack_page_count)
        .expect("user process VA allocations should not overlap, and the page table should check");

    // create process communication queues
    let send_qu = OwnedQueue::new(FIRST_SEND_QUEUE_ID, 2).context(error::MemorySnafu {
        reason: "allocate first send queue",
    })?;
    let recv_qu = OwnedQueue::new(FIRST_RECV_QUEUE_ID, 2).context(error::MemorySnafu {
        reason: "allocate first receive queue",
    })?;
    let (send_qu_addr, send_qu_size, recv_qu_addr, recv_qu_size) =
        attach_queues(&send_qu, &recv_qu, &mut pt, &mut address_space_allocator)?;
    let queues = ConcurrentLinkedList::default();
    queues.push((Arc::new(send_qu), Arc::new(recv_qu)));

    // log::debug!("process page table: {pt:?}");

    // create process structure
    let proc = Arc::new(Process {
        id: pid,
        page_tables: pt,
        threads: Default::default(),
        next_queue_id: AtomicU16::new((FIRST_RECV_QUEUE_ID.saturating_add(1)).into()),
        queues,
        address_space_allocator,
        exit_code: Once::new(),
        exit_waker: spin::Mutex::new(Vec::new()),
    });
    processes().push(proc.clone());

    // create main thread
    let tid = next_thread_id();
    let start_regs =
        Registers::from_args(&[send_qu_addr.0, send_qu_size, recv_qu_addr.0, recv_qu_size]);
    let main_thread = Arc::new(Thread::user_thread(
        proc.clone(),
        tid,
        VirtualAddress(bin.ehdr.e_entry as usize),
        stack_vaddr.add((stack_page_count * PAGE_SIZE) - 64),
        ThreadPriority::Normal,
        start_regs,
    ));
    proc.threads.lock_blocking().push(main_thread.clone());
    threads().push(main_thread.clone());

    // run any custom code before the process is eligible to be scheduled but after it has been created.
    // this allows the caller to attach resources that requires .awaiting etc.
    if let Some(b) = before_launch {
        b(proc.clone());
    }

    // schedule thread 0 for process
    thread::scheduler::scheduler().add_thread(main_thread);

    Ok(proc)
}
