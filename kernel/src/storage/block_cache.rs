use super::*;
use crate::memory::{
    virtual_address_allocator, PhysicalAddress, PhysicalBuffer, VirtualAddress, PAGE_SIZE,
};
use crate::tasks::locks::{Mutex, RwLock};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_trait::async_trait;
use bitfield::{bitfield, BitRange};
use derive_more::Display;
use futures::future;
use hashbrown::HashMap;
use snafu::{ResultExt, Snafu};

/*
 * for each loaded block we need to know
 * - virtual address of block copy in memory
 * - dirty flag
 * - locked flag
 *
 * additionally we need to keep track of allocated physical pages
 */

// logical addr: |---tag---|---chunk id---|---block offset---|
// chunk id -> index into metadata array
// tag -> used to determine if the chunk is loaded
// block offset -> offset into chunk to locate block

bitfield! {
    #[derive(Copy, Clone, Default)]
    struct ChunkMetadata(u64);
    impl Debug;
    tag, set_tag: 63, 60;
    bool, occupied, set_occupied: 1;
    bool, dirty, set_dirty: 0;
}

/// A BlockCache provides a cache layer on top of a BlockStore, allowing for unaligned reads/writes and minimizing unneccessary underlying operations
///
/// A BlockCache is internally synchronized and thus safe to share between threads or tasks.
pub struct BlockCache {
    store: Mutex<Box<dyn BlockStore>>,
    block_offset_bits: usize,
    chunk_id_bits: usize,
    block_size: usize,
    chunk_size: usize,
    blocks_per_chunk: usize,
    num_chunks: usize,
    metadata: RwLock<Vec<ChunkMetadata>>,
    buffer: RwLock<PhysicalBuffer>,
    buffer_pa: PhysicalAddress,
}

// TODO: bounds checking on device size
impl BlockCache {
    /// Create a new block cache using `store` as the underlying block store, caching a maximum of
    /// `max_size_in_pages` pages worth of blocks in memory.
    pub fn new(
        store: impl BlockStore + 'static,
        max_size_in_pages: usize,
    ) -> Result<BlockCache, Error> {
        let block_size = store.supported_block_size();
        let (blocks_per_chunk, num_chunks, chunk_size) = if block_size < PAGE_SIZE {
            (
                PAGE_SIZE / block_size,
                // 1 chunk per page
                max_size_in_pages,
                PAGE_SIZE,
            )
        } else {
            // TODO: for now, assume that the block size is still a multiple of the page size
            assert_eq!(block_size % PAGE_SIZE, 0);
            (1, max_size_in_pages / block_size, block_size)
        };
        let chunk_id_bits = num_chunks.ilog2() as usize;
        let block_offset_bits = blocks_per_chunk.ilog2() as usize;
        log::debug!("creating block cache with {num_chunks} chunks, {blocks_per_chunk} blocks per chunk, {chunk_id_bits} bits for chunk ID, {block_offset_bits} bits for block offset");
        let buffer = PhysicalBuffer::alloc(max_size_in_pages, &Default::default())
            .context(super::MemorySnafu)?;
        Ok(BlockCache {
            store: Mutex::new(Box::new(store)),
            block_offset_bits,
            blocks_per_chunk,
            chunk_id_bits,
            block_size,
            chunk_size,
            num_chunks,
            metadata: RwLock::new(
                core::iter::repeat_with(Default::default)
                    .take(num_chunks)
                    .collect(),
            ),
            buffer_pa: buffer.physical_address(),
            // TODO: in the future, the physical memory allocator should support loaning pages to
            // caches etc but for now we just allocate the entire cache upfront
            buffer: RwLock::new(buffer),
        })
    }

    /// Size of a single block in bytes (cached value of [BlockStore::supported_block_size]).
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Decompose a logical address into its cache tag, chunk ID and block offset, respectively.
    fn decompose_address(&self, address: BlockAddress) -> (u64, u64, u64) {
        let tag = address
            .0
            .bit_range(63, self.chunk_id_bits + self.block_offset_bits);
        let chunk_id = address.0.bit_range(
            self.chunk_id_bits + self.block_offset_bits,
            self.block_offset_bits,
        );
        let block_offset = address.0.bit_range(self.block_offset_bits - 1, 0);
        // log::trace!("decomposing {:b} => {tag:b}:{chunk_id:b}:{block_offset:b}", address.0);
        (tag, chunk_id, block_offset)
    }

    fn compose_address(&self, tag: u64, chunk_id: u64) -> BlockAddress {
        BlockAddress(
            (tag << (self.chunk_id_bits + self.block_offset_bits))
                | (chunk_id << self.block_offset_bits),
        )
    }

    fn chunk_phy_addr(&self, chunk_id: u64) -> PhysicalAddress {
        self.buffer_pa
            .offset((chunk_id * self.chunk_size as u64) as isize)
    }

    async fn load_chunk(&self, tag: u64, chunk_id: u64, mark_dirty: bool) -> Result<(), Error> {
        let md = { self.metadata.read().await[chunk_id as usize] };
        log::trace!("loading chunk {tag}:{chunk_id}, md={md:?}");
        if !md.occupied() || md.tag() != tag {
            // chunk is not present
            let mut store = self.store.lock().await;
            if md.dirty() {
                store
                    .write_blocks(
                        self.chunk_phy_addr(chunk_id),
                        self.compose_address(md.tag(), chunk_id),
                        self.blocks_per_chunk,
                    )
                    .await?;
            }
            store
                .read_blocks(
                    self.compose_address(tag, chunk_id),
                    self.chunk_phy_addr(chunk_id),
                    self.blocks_per_chunk,
                )
                .await?;
            let mut md = &mut self.metadata.write().await[chunk_id as usize];
            md.set_tag(tag);
            md.set_occupied(true);
            md.set_dirty(mark_dirty);
        }
        Ok(())
    }

    async fn load_chunks(
        &self,
        starting_tag: u64,
        starting_chunk_id: u64,
        num_chunks_inclusive: u64,
        mark_dirty: bool,
    ) -> Result<(), Error> {
        let res = future::join_all((0..num_chunks_inclusive).map(|i| {
            let mut tag = starting_tag;
            let mut chunk_id = starting_chunk_id + i;
            if chunk_id > self.num_chunks as u64 {
                tag += chunk_id / self.num_chunks as u64;
                chunk_id = chunk_id % self.num_chunks as u64;
            }
            self.load_chunk(tag, chunk_id, mark_dirty)
        }))
        .await;
        for e in res.into_iter().flat_map(Result::err) {
            return Err(e);
        }
        Ok(())
    }

    /// Copy bytes from the cache into a slice. Any unloaded blocks will be loaded, and copies can span multiple blocks.
    pub async fn copy_bytes(
        &self,
        mut address: BlockAddress,
        mut byte_offset: usize,
        dest: &mut [u8],
    ) -> Result<(), Error> {
        if byte_offset >= self.block_size() {
            address.0 += (byte_offset / self.block_size()) as u64;
            byte_offset = byte_offset % self.block_size();
        }

        let (starting_tag, starting_chunk_id, initial_block_offset) =
            self.decompose_address(address);

        log::trace!("copying from {starting_tag}:{starting_chunk_id}:{initial_block_offset} + {byte_offset} [{address}]");
        let num_chunks_inclusive = dest.len().div_ceil(self.chunk_size);
        if num_chunks_inclusive > self.num_chunks {
            // if the read is larger than the entire cache
            // TODO: implement reads larger than the size of the cache by directly issuing a read
            // that writes to the destination buffer
            todo!("implement large reads");
        }
        self.load_chunks(
            starting_tag,
            starting_chunk_id,
            num_chunks_inclusive as u64,
            false,
        )
        .await?;
        let offset = (starting_chunk_id as usize * self.chunk_size
            + initial_block_offset as usize * self.block_size) as usize
            + byte_offset;
        dest.copy_from_slice(&self.buffer.read().await.as_bytes()[offset..offset + dest.len()]);
        Ok(())
    }

    /// Write bytes from a slice into the cache. Any unloaded blocks will be loaded, and writes can span multiple blocks. The cache will write the blocks back to the underlying storage when they are ejected from the cache.
    pub async fn write_bytes(
        &self,
        mut address: BlockAddress,
        mut byte_offset: usize,
        src: &[u8],
    ) -> Result<(), Error> {
        if byte_offset >= self.block_size() {
            address.0 += (byte_offset / self.block_size()) as u64;
            byte_offset = byte_offset % self.block_size();
        }

        let (starting_tag, starting_chunk_id, initial_block_offset) =
            self.decompose_address(address);
        log::trace!("writing to {starting_tag}:{starting_chunk_id}:{initial_block_offset}");
        let num_chunks_inclusive = src.len().div_ceil(self.chunk_size);
        if num_chunks_inclusive > self.num_chunks {
            // if the write is larger than the entire cache
            // TODO: implement writes larger than the size of the cache by writing the ends using
            // the cache (to take care of offsets) and then issuing a big direct write for the rest
            todo!("implement large writes");
        }
        self.load_chunks(
            starting_tag,
            starting_chunk_id,
            num_chunks_inclusive as u64,
            true,
        )
        .await?;
        let offset = (starting_chunk_id as usize * self.chunk_size
            + initial_block_offset as usize * self.block_size) as usize
            + byte_offset;
        self.buffer.write().await.as_bytes_mut()[offset..offset + src.len()].copy_from_slice(src);
        Ok(())
    }

    /// Update bytes in the cache in a certain range. All blocks in the range will be loaded into the cache if they are unloaded.
    pub async fn update_bytes(
        &self,
        address: BlockAddress,
        size_in_bytes: usize,
        f: impl FnOnce(&mut [u8]),
    ) {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::tasks::block_on;

    struct MockReadOnlyBlockStore {
        block_size: usize,
        read_counter: Arc<AtomicUsize>,
    }

    impl MockReadOnlyBlockStore {
        fn new(block_size: usize) -> Self {
            Self {
                block_size,
                read_counter: Arc::new(AtomicUsize::default()),
            }
        }
    }

    #[async_trait]
    impl BlockStore for MockReadOnlyBlockStore {
        fn supported_block_size(&self) -> usize {
            self.block_size
        }
        async fn read_blocks(
            &mut self,
            source_addr: BlockAddress,
            destination_addr: PhysicalAddress,
            num_blocks: usize,
        ) -> Result<usize, Error> {
            unsafe {
                let p: *mut u8 = destination_addr.to_virtual_canonical().as_ptr();
                let mut s = core::slice::from_raw_parts_mut(p, self.block_size * num_blocks);
                for (i, d) in s.iter_mut().enumerate() {
                    *d = (i % 255) as u8;
                }
            }
            self.read_counter.fetch_add(num_blocks, Ordering::Release);
            Ok(num_blocks)
        }
        async fn write_blocks(
            &mut self,
            source_addr: PhysicalAddress,
            destination_addr: BlockAddress,
            num_blocks: usize,
        ) -> Result<usize, Error> {
            unimplemented!()
        }
    }

    #[test_case]
    fn cold_read_single_block() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let mut c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        block_on(c.copy_bytes(BlockAddress(0), 0, &mut buf)).unwrap();
        assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }

    #[test_case]
    fn cold_read_across_blocks_small() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let mut c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        block_on(c.copy_bytes(BlockAddress(0), 4, &mut buf)).unwrap();
        assert_eq!(buf, [4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(rc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }

    #[test_case]
    fn warm_read_single_block() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let mut c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        // first read cold
        block_on(c.copy_bytes(BlockAddress(0), 0, &mut buf)).unwrap();
        assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rc.swap(0, Ordering::Acquire), PAGE_SIZE / 8);
        // second read should just be cached
        buf.fill(0);
        block_on(c.copy_bytes(BlockAddress(0), 0, &mut buf)).unwrap();
        assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rc.load(Ordering::Acquire), 0);
    }

    struct MockRwBlockStore {
        buffer: Vec<u8>,
        read_counter: Arc<AtomicUsize>,
        write_counter: Arc<AtomicUsize>,
    }

    impl MockRwBlockStore {
        fn new() -> Self {
            MockRwBlockStore {
                buffer: [0; 4096].into(),
                read_counter: Default::default(),
                write_counter: Default::default(),
            }
        }
    }

    #[async_trait]
    impl BlockStore for MockRwBlockStore {
        fn supported_block_size(&self) -> usize {
            8
        }

        async fn read_blocks(
            &mut self,
            source_addr: BlockAddress,
            destination_addr: PhysicalAddress,
            num_blocks: usize,
        ) -> Result<usize, Error> {
            log::debug!("read {source_addr} → {destination_addr}, {num_blocks} blocks");
            unsafe {
                let p: *mut u8 = destination_addr.to_virtual_canonical().as_ptr();
                let mut s = core::slice::from_raw_parts_mut(p, 8 * num_blocks);
                let start = (source_addr.0 * 8) as usize;
                let end = start + num_blocks * 8;
                if start > self.buffer.len() || end > self.buffer.len() {
                    return Ok(0);
                }
                s.copy_from_slice(&self.buffer[start..end]);
            }
            self.read_counter.fetch_add(num_blocks, Ordering::Release);
            Ok(num_blocks)
        }

        async fn write_blocks(
            &mut self,
            source_addr: PhysicalAddress,
            destination_addr: BlockAddress,
            num_blocks: usize,
        ) -> Result<usize, Error> {
            log::debug!("write {source_addr} → {destination_addr}, {num_blocks} blocks");
            unsafe {
                let p: *mut u8 = source_addr.to_virtual_canonical().as_ptr();
                let mut s = core::slice::from_raw_parts_mut(p, 8 * num_blocks);
                let start = (destination_addr.0 * 8) as usize;
                let end = start + num_blocks * 8;
                self.buffer[start..end].copy_from_slice(s);
            }
            self.write_counter.fetch_add(num_blocks, Ordering::Release);
            Ok(num_blocks)
        }
    }

    #[test_case]
    fn write_and_read_back() {
        let bs = MockRwBlockStore::new();
        let (rc, wc) = (bs.read_counter.clone(), bs.write_counter.clone());
        let mut c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        buf.copy_from_slice(b"testtest");
        block_on(c.write_bytes(BlockAddress(7), 0, &buf));
        assert_eq!(rc.swap(0, Ordering::Acquire), PAGE_SIZE / 8);
        assert_eq!(wc.load(Ordering::Acquire), 0); // no writes yet
        buf.fill(0);
        block_on(c.copy_bytes(BlockAddress(7), 0, &mut buf)).unwrap();
        assert_eq!(&buf, b"testtest");
        assert_eq!(rc.load(Ordering::Acquire), 0);
        assert_eq!(wc.load(Ordering::Acquire), 0);
    }

    #[test_case]
    fn write_back() {
        let bs = MockRwBlockStore::new();
        let (rc, wc) = (bs.read_counter.clone(), bs.write_counter.clone());
        let mut c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        buf.copy_from_slice(b"testtest");
        block_on(c.write_bytes(BlockAddress(7), 0, &buf));
        assert_eq!(rc.swap(0, Ordering::Acquire), PAGE_SIZE / 8);
        assert_eq!(wc.load(Ordering::Acquire), 0); // no writes yet
        buf.fill(0);
        // read a block with a different tag to cause the cache to write the dirty block out
        block_on(c.copy_bytes(BlockAddress(0x800), 0, &mut buf)).unwrap();
        assert_eq!(rc.load(Ordering::Acquire), 0);
        assert_eq!(wc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }
}
