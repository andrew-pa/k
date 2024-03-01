use elf::segment::ProgramHeader;

use super::*;

/// An error that can occur trying to spawn a process.
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

fn load_segment(
    seg: &ProgramHeader,
    src_data: &[u8],
    pt: &mut PageTable,
    address_space_allocator: &mut VirtualAddressAllocator,
) -> Result<(), SpawnError> {
    // TODO: we are doing this wrong. the init ELF file seems to make the alignment_offset end up
    // being the running total size of all segments so far, causing each new segment to have to
    // allocate padding space the size of all previous segments. This is obviously unnecessary.

    log::trace!("mapping segment {seg:x?}");
    let page_aligned_vaddr = VirtualAddress((seg.p_vaddr as usize) & !(PAGE_SIZE - 1));
    let page_alignment_offset = (seg.p_vaddr as usize) & (PAGE_SIZE - 1);
    let page_count = (seg.p_memsz as usize + page_alignment_offset).div_ceil(PAGE_SIZE);
    let mut dest_memory_segment =
        PhysicalBuffer::alloc(page_count, &Default::default()).context(MemorySnafu {
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
    .context(MemoryMapSnafu)?;
    address_space_allocator
        .reserve(page_aligned_vaddr, page_count)
        .expect("user process VA allocations should not overlap, and the page table should check");
    Ok(())
}

/// Make a channel available to the process via mapped memory.
/// The submission and completion queues will be layed out consecutively in memory, and mapping will be created immediately.
/// Returns the base address and the total length in bytes of the mapped region for the submission and completion queues, respectively.
pub fn attach_channel(
    channel: &Channel,
    page_tables: &mut PageTable,
    address_space_allocator: &mut VirtualAddressAllocator,
) -> Result<(VirtualAddress, usize, usize), SpawnError> {
    let sub_buf = channel.submission_queue_buffer();
    let com_buf = channel.completion_queue_buffer();
    let total_len = sub_buf.len() + com_buf.len();
    let num_pages = total_len.div_ceil(PAGE_SIZE);
    let base_addr = address_space_allocator
        .alloc(num_pages)
        .context(MemorySnafu {
            reason: "allocate in process address space for channel",
        })?;
    let map_opts = PageTableEntryOptions {
        read_only: false,
        el0_access: true,
    };
    page_tables
        .map_range(
            sub_buf.physical_address(),
            base_addr,
            sub_buf.page_count(),
            true,
            &map_opts,
        )
        .context(MemoryMapSnafu)?;
    page_tables
        .map_range(
            com_buf.physical_address(),
            base_addr.add(sub_buf.len()),
            com_buf.page_count(),
            true,
            &map_opts,
        )
        .context(MemoryMapSnafu)?;
    Ok((base_addr, sub_buf.len(), com_buf.len()))
}

/// Creates a new process from a binary loaded from a path in the registry.
/// Supports ELF binaries.
pub async fn spawn_process(
    binary_path: impl AsRef<crate::registry::Path>,
    before_launch: Option<impl FnOnce(&mut Process)>,
) -> Result<ProcessId, SpawnError> {
    use crate::registry::registry;

    // load & parse binary
    let path = binary_path.as_ref();
    log::debug!("spawning process with binary file at {path}");
    let mut f = registry()
        .open_file(path)
        .await
        .with_context(|_| RegistrySnafu { path })?;

    let f_len = f.len() as usize;
    log::debug!("binary file size = {f_len}");
    let src_data = PhysicalBuffer::alloc(f_len.div_ceil(PAGE_SIZE), &Default::default()).context(
        MemorySnafu {
            reason: "allocate temporary buffer for binary",
        },
    )?;
    f.load_pages(0, src_data.physical_address(), src_data.page_count())
        .await
        .context(FileSystemSnafu {
            reason: "read binary from disk",
        })?;

    // parse ELF binary
    let bin: elf::ElfBytes<elf::endian::LittleEndian> =
        elf::ElfBytes::minimal_parse(src_data.as_bytes())
            .map_err(|reason| BinFormatSnafu { reason }.build())?;
    // log::debug!("ELF header = {:#x?}", bin.ehdr);

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

    // create an address space allocator for the entire address space of the process
    let mut address_space_allocator =
        VirtualAddressAllocator::new(VirtualAddress(PAGE_SIZE), 0x0000_ffff_ffff_ffff / PAGE_SIZE);

    let segments = bin.segments().context(OtherSnafu {
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
            .context(MemorySnafu {
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
    .context(MemoryMapSnafu)?;
    address_space_allocator
        .reserve(stack_vaddr, stack_page_count)
        .expect("user process VA allocations should not overlap, and the page table should check");

    // create process communication channel
    let channel = Channel::new(2, 2).context(MemorySnafu {
        reason: "allocate channel",
    })?;
    let (channel_base_addr, channel_sub_size, channel_com_size) =
        attach_channel(&channel, &mut pt, &mut address_space_allocator)?;

    log::debug!("process page table: {pt:?}");

    let tid = next_thread_id();

    // create process structure
    let mut proc = Process {
        id: pid,
        page_tables: pt,
        threads: smallvec![tid],
        mapped_files: HashMap::new(),
        channel,
        address_space_allocator,
    };

    // run any custom code before the process is eligible to be scheduled but after it has been created.
    // this allows the caller to attach resources that requires .awaiting etc.
    if let Some(b) = before_launch {
        b(&mut proc);
    }

    let p = processes().insert_blocking(pid, proc);
    assert!(p.is_none());

    let start_regs =
        Registers::from_args(&[channel_base_addr.0, channel_sub_size, channel_com_size]);

    // create thread 0 for process
    spawn_thread(Thread::user_thread(
        pid,
        tid,
        VirtualAddress(bin.ehdr.e_entry as usize),
        stack_vaddr.add((stack_page_count * PAGE_SIZE) - 64),
        ThreadPriority::Normal,
        start_regs,
    ));

    Ok(pid)
}
