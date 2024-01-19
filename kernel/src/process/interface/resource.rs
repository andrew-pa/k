use alloc::boxed::Box;
use snafu::{ResultExt, Snafu};

use crate::{
    fs::File,
    memory::{
        paging::{PageTable, PageTableEntryOptions},
        physical_memory_allocator, VirtualAddress, PAGE_SIZE,
    },
};

#[derive(Debug, Snafu)]
pub enum HandlePageFaultError {
    Memory {
        source: crate::memory::MemoryError,
    },
    Map {
        source: crate::memory::paging::MapError,
    },
    FileSystem {
        source: crate::fs::Error,
    },
}

pub struct MappedFile {
    pub length_in_bytes: usize,
    pub resource: Box<dyn File>,
}

impl MappedFile {
    pub async fn on_page_fault(
        &mut self,
        base_address: VirtualAddress,
        accessed_address: VirtualAddress,
        page_tables: &mut PageTable,
    ) -> Result<(), HandlePageFaultError> {
        // compute address of affected page
        let accessed_page = VirtualAddress(accessed_address.0 & !(PAGE_SIZE - 1));
        log::trace!("handling page fault in mapped file at {base_address}, accessed address = {accessed_address} ({accessed_page})");
        // allocate new memory
        log::trace!("allocate new memory");
        let dest_address = { physical_memory_allocator().alloc().context(MemorySnafu)? };
        // map memory in page tables
        log::trace!("map new memory in process page table");
        page_tables
            .map_range(
                dest_address,
                accessed_page,
                1,
                true,
                &PageTableEntryOptions {
                    read_only: false,
                    el0_access: true,
                },
            )
            .context(MapSnafu)?;
        // load data from file
        log::trace!("load actual data into memory");
        self.resource
            .load_pages((accessed_page.0 - base_address.0) as u64, dest_address, 1)
            .await
            .context(FileSystemSnafu)
    }
}

pub fn resource_maps(base_addr: &VirtualAddress, res: &MappedFile, addr: VirtualAddress) -> bool {
    addr.0
        .checked_sub(base_addr.0)
        .map(|i| i < res.length_in_bytes)
        .unwrap_or_default()
}
