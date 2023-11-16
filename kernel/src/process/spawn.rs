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

/// Creates a new process from a binary loaded from a path in the registry.
/// Supports ELF binaries.
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

    let segments = bin.segments().context(OtherSnafu {
        reason: "expected binary to have at least one segment",
    })?;

    for seg in segments {
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
        // TODO: set correct page flags beyond R/W, perhaps also parse p_flags more rigorously
        pt.map_range(
            pa,
            aligned_vaddr,
            (seg.p_memsz as usize).div_ceil(PAGE_SIZE),
            true,
            &PageTableEntryOptions {
                read_only: !seg.p_flags.bit(1),
                el0_access: true,
            },
        )
        .context(MemoryMapSnafu)?;
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
    .context(MemoryMapSnafu);

    log::debug!("process page table: {pt:?}");

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
        VirtualAddress(bin.ehdr.e_entry as usize),
        stack_vaddr.offset((stack_page_count * PAGE_SIZE) as isize - 64),
        ThreadPriority::Normal,
    ));

    Ok(pid)
}
