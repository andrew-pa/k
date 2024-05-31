use super::*;
use crate::error;
use crate::memory::{PhysicalBuffer, PAGE_SIZE};
use crate::tasks::locks::Mutex;
use alloc::vec::Vec;

use bitfield::{bitfield, BitRange};

use futures::future;

use snafu::ResultExt;

// TODO: we could entirely get rid of this and use the MMU instead, given that all block
// stores could be addressed within the kernel's address space.
// what we would have is a new heap space that was mapped in the kernel that you could allocate in
// and associate with some block device and then it's just typical memory paging type thing with
// page faults and loading pages on demand, same as user space.

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
    tag, set_tag: 63, 2;
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
    metadata: Vec<Mutex<ChunkMetadata>>,
    /// The actual buffer used for caching. This is synchronized per-chunk via the `metadata` locks.
    buffer: PhysicalBuffer,
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
        let buffer = PhysicalBuffer::alloc(max_size_in_pages, &Default::default()).context(
            error::MemorySnafu {
                reason: "allocate backing store for block cache",
            },
        )?;
        Ok(BlockCache {
            store: Mutex::new(Box::new(store)),
            block_offset_bits,
            blocks_per_chunk,
            chunk_id_bits,
            block_size,
            chunk_size,
            num_chunks,
            metadata: core::iter::repeat_with(Default::default)
                .take(num_chunks)
                .collect(),
            buffer_pa: buffer.physical_address(),
            // TODO: in the future, the physical memory allocator should support loaning pages to
            // caches etc but for now we just allocate the entire cache upfront
            buffer,
        })
    }

    /// Size of a single block in bytes (cached value of [BlockStore::supported_block_size]).
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn underlying_store(&self) -> &Mutex<Box<dyn BlockStore>> {
        &self.store
    }

    /// Decompose a logical address into its cache tag, chunk ID and block offset, respectively.
    fn decompose_address(&self, address: BlockAddress) -> (u64, u64, u64) {
        let tag = address
            .0
            .bit_range(63, self.chunk_id_bits + self.block_offset_bits);
        let chunk_id = if self.chunk_id_bits == 0 {
            0
        } else {
            address.0.bit_range(
                self.chunk_id_bits + self.block_offset_bits - 1,
                self.block_offset_bits,
            )
        };
        let block_offset = address.0.bit_range(self.block_offset_bits - 1, 0);
        /*log::trace!(
        "decomposing {:b} => {tag:b}:{chunk_id:b}:{block_offset:b}",
        address.0
        );*/
        assert!(
            (chunk_id as usize) < self.num_chunks,
            "chunk id {chunk_id} >= # chunks {}",
            self.num_chunks
        );
        assert!((block_offset as usize) < self.block_size);
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

    /// Update a single cache chunk to ensure that it holds data from a block at `compose_address(tag, chunk_id)` and copy some memory while the chunk is locked.
    ///
    /// The chunk in the cache for `chunk_id` will be replaced with the contents of the block at
    /// `compose_address(tag, chunk_id)`.
    /// If the chunk is dirty, it will be written to storage before being replaced.
    /// The chunk metadata is written to record the new tag, occupation and dirtiness if it has changed.
    /// Bytes from the `src` will be copied to the `dst` (both buffers must have the same length),
    /// regardless if any IO has taken place. The bytes are copied while the lock on the chunk is
    /// held, so it is guarenteed that the memory for the chunk in the cache buffer will contain
    /// the bytes of the block referred to by `compose_address(tag, chunk_id)`.
    async fn update_single_chunk(
        &self,
        tag: u64,
        chunk_id: u64,
        mark_dirty: bool,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<(), StorageError> {
        let mut md = self.metadata[chunk_id as usize].lock().await;
        if !md.occupied() || md.tag() != tag {
            // chunk is not present
            log::trace!("loading chunk {tag}:{chunk_id}, md={:?}", *md);
            let mut store = self.store.lock().await;
            if md.dirty() {
                store
                    .write_blocks(
                        &[(self.chunk_phy_addr(chunk_id), self.blocks_per_chunk)],
                        self.compose_address(md.tag(), chunk_id),
                    )
                    .await?;
            }
            store
                .read_blocks(
                    self.compose_address(tag, chunk_id),
                    &[(self.chunk_phy_addr(chunk_id), self.blocks_per_chunk)],
                )
                .await?;
            md.set_tag(tag);
            md.set_occupied(true);
            md.set_dirty(mark_dirty);
        }
        // do the actual copy - it's important to make sure the metadata stays locked for this so
        // that another thread doesn't decide to repurpose this chunk while we are copying.
        dst.copy_from_slice(src);
        Ok(())
    }

    /// Process `num_chunks_inclusive` chunks in the cache starting at `(starting_tag, starting_chunk_id)` in parallel using [Self::process_single_chunk].
    /// If `num_chunks_inclusive` is greater than [Self::num_chunks], then the tag will wrap.
    ///
    /// The `src` and `dst` will be divided into chunks of [Self::chunk_size] bytes.
    /// To work correctly, they must be chunk-aligned if they are slices into the cache buffer.
    /// An offset `initial_offset` can be applied to the first chunk to account for skipping some number of blocks/bytes. For reads, this will be the `src` buffer, and for writes it will be the `dst` buffer.
    async fn process_chunks(
        &self,
        (starting_tag, starting_chunk_id): (u64, u64),
        num_chunks_inclusive: u64,
        is_write: bool,
        src: &[u8],
        dst: &mut [u8],
        initial_offset: usize,
    ) -> Result<(), StorageError> {
        future::try_join_all(
            (0..num_chunks_inclusive)
                .zip(src.chunks(self.chunk_size))
                .zip(dst.chunks_mut(self.chunk_size))
                .map(|((i, src), dst)| {
                    let mut tag = starting_tag;
                    let mut chunk_id = starting_chunk_id + i;
                    if chunk_id >= self.num_chunks as u64 {
                        tag += chunk_id / self.num_chunks as u64;
                        chunk_id %= self.num_chunks as u64;
                    }
                    let len = if is_write { src.len() } else { dst.len() };
                    let src = if !is_write && i == 0 {
                        &src[initial_offset..(initial_offset + len)]
                    } else {
                        &src[0..len]
                    };
                    let dst = if is_write && i == 0 {
                        &mut dst[initial_offset..(initial_offset + len)]
                    } else {
                        &mut dst[0..len]
                    };
                    self.update_single_chunk(tag, chunk_id, is_write, src, dst)
                }),
        )
        .await
        .map(|_| ()) //throw away the silly vector of units TODO: can we not collect the units?
    }

    /// Copy bytes from the cache into a slice. Any unloaded blocks will be loaded, and copies can span multiple blocks.
    pub async fn copy_bytes(
        &self,
        mut address: BlockAddress,
        mut byte_offset: usize,
        dst: &mut [u8],
    ) -> Result<(), StorageError> {
        if byte_offset >= self.block_size() {
            address.0 += (byte_offset / self.block_size()) as u64;
            byte_offset %= self.block_size();
        }

        let (mut tag, mut chunk_id, mut block_offset) = self.decompose_address(address);

        let mut num_chunks_inclusive = dst.len().div_ceil(self.chunk_size);
        // log::trace!("copying from {starting_tag}:{starting_chunk_id}:{initial_block_offset} + {byte_offset} len={},chunk count={} [{address}]", dest.len(), num_chunks_inclusive);
        while num_chunks_inclusive > 0 {
            // we can process in a single call at most the number of chunks between where we start in the cache buffer and the end of the buffer
            let batch_num_chunks = num_chunks_inclusive.min(self.num_chunks - chunk_id as usize);

            self.process_chunks(
                (tag, chunk_id),
                batch_num_chunks as u64,
                false,
                &self.buffer.as_bytes()[chunk_id as usize * self.chunk_size..],
                dst,
                block_offset as usize * self.block_size + byte_offset,
            )
            .await?;

            num_chunks_inclusive -= batch_num_chunks;
            chunk_id += batch_num_chunks as u64;
            if chunk_id >= self.num_chunks as u64 {
                tag += chunk_id / self.num_chunks as u64;
                chunk_id %= self.num_chunks as u64;
            }
            block_offset = 0;
        }

        Ok(())
    }

    /// Write bytes from a slice into the cache. Any unloaded blocks will be loaded, and writes can span multiple blocks. The cache will write the blocks back to the underlying storage when they are ejected from the cache.
    pub async fn write_bytes(
        &self,
        mut address: BlockAddress,
        mut byte_offset: usize,
        src: &[u8],
    ) -> Result<(), StorageError> {
        if byte_offset >= self.block_size() {
            address.0 += (byte_offset / self.block_size()) as u64;
            byte_offset %= self.block_size();
        }

        let (mut tag, mut chunk_id, mut block_offset) = self.decompose_address(address);

        let mut num_chunks_inclusive = src.len().div_ceil(self.chunk_size);
        // log::trace!("copying from {starting_tag}:{starting_chunk_id}:{initial_block_offset} + {byte_offset} len={},chunk count={} [{address}]", dest.len(), num_chunks_inclusive);
        while num_chunks_inclusive > 0 {
            // we can process in a single call at most the number of chunks between where we start in the cache buffer and the end of the buffer
            let batch_num_chunks = num_chunks_inclusive.min(self.num_chunks - chunk_id as usize);

            let dst = unsafe {
                // SAFETY: we use the chunk locks to synchronize shared access to the cache buffer, so this is fine.
                &mut self.buffer.as_bytes_mut_force_unsafe()[chunk_id as usize * self.chunk_size..]
            };

            self.process_chunks(
                (tag, chunk_id),
                batch_num_chunks as u64,
                true,
                src,
                dst,
                block_offset as usize * self.block_size + byte_offset,
            )
            .await?;

            num_chunks_inclusive -= batch_num_chunks;
            chunk_id += batch_num_chunks as u64;
            if chunk_id >= self.num_chunks as u64 {
                tag += chunk_id / self.num_chunks as u64;
                chunk_id %= self.num_chunks as u64;
            }
            block_offset = 0;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use alloc::sync::Arc;

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
        async fn read_blocks<'a>(
            &mut self,
            source_addr: BlockAddress,
            destinations: &'a [(PhysicalAddress, usize)],
        ) -> Result<usize, StorageError> {
            let mut total = 0;
            for (destination_addr, num_blocks) in destinations {
                unsafe {
                    let p: *mut u8 = destination_addr.to_virtual_canonical().as_ptr();
                    let s = core::slice::from_raw_parts_mut(p, self.block_size * num_blocks);
                    for (i, d) in s.iter_mut().enumerate() {
                        *d = (i % 255) as u8;
                    }
                }
                self.read_counter.fetch_add(*num_blocks, Ordering::Release);
                total += num_blocks;
            }
            Ok(total)
        }

        async fn write_blocks<'a>(
            &mut self,
            source_addrs: &'a [(PhysicalAddress, usize)],
            destination_addr: BlockAddress,
        ) -> Result<usize, StorageError> {
            unimplemented!()
        }
    }

    #[test_case]
    fn cold_read_single_block() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        block_on(c.copy_bytes(BlockAddress(0), 0, &mut buf)).unwrap();
        assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }

    #[test_case]
    fn cold_read_across_blocks_small() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        block_on(c.copy_bytes(BlockAddress(0), 4, &mut buf)).unwrap();
        assert_eq!(buf, [4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(rc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }

    #[test_case]
    fn warm_read_single_block() {
        let bs = MockReadOnlyBlockStore::new(8);
        let rc = bs.read_counter.clone();
        let c = BlockCache::new(bs, 1).unwrap();
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

        async fn read_blocks<'a>(
            &mut self,
            source_addr: BlockAddress,
            destinations: &'a [(PhysicalAddress, usize)],
        ) -> Result<usize, StorageError> {
            log::debug!("read {source_addr} → {destinations:?}");
            let mut total = 0;
            for (destination_addr, num_blocks) in destinations {
                unsafe {
                    let p: *mut u8 = destination_addr.to_virtual_canonical().as_ptr();
                    let s = core::slice::from_raw_parts_mut(p, 8 * num_blocks);
                    let start = (source_addr.0 * 8) as usize;
                    let end = start + num_blocks * 8;
                    if start > self.buffer.len() || end > self.buffer.len() {
                        return Ok(0);
                    }
                    s.copy_from_slice(&self.buffer[start..end]);
                }
                self.read_counter.fetch_add(*num_blocks, Ordering::Release);
                total += num_blocks;
            }
            Ok(total)
        }

        async fn write_blocks<'a>(
            &mut self,
            sources: &'a [(PhysicalAddress, usize)],
            destination_addr: BlockAddress,
        ) -> Result<usize, StorageError> {
            log::debug!("write {sources:?} → {destination_addr}");
            let mut total = 0;
            for (source_addr, num_blocks) in sources {
                unsafe {
                    let p: *mut u8 = source_addr.to_virtual_canonical().as_ptr();
                    let s = core::slice::from_raw_parts_mut(p, 8 * num_blocks);
                    let start = (destination_addr.0 * 8) as usize;
                    let end = start + num_blocks * 8;
                    self.buffer[start..end].copy_from_slice(s);
                }
                self.write_counter.fetch_add(*num_blocks, Ordering::Release);
                total += num_blocks;
            }
            Ok(total)
        }
    }

    #[test_case]
    fn write_and_read_back() {
        let bs = MockRwBlockStore::new();
        let (rc, wc) = (bs.read_counter.clone(), bs.write_counter.clone());
        let c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        buf.copy_from_slice(b"testtest");
        block_on(c.write_bytes(BlockAddress(7), 0, &buf)).unwrap();
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
        let c = BlockCache::new(bs, 1).unwrap();
        let mut buf = [0; 8];
        buf.copy_from_slice(b"testtest");
        block_on(c.write_bytes(BlockAddress(7), 0, &buf)).unwrap();
        assert_eq!(rc.swap(0, Ordering::Acquire), PAGE_SIZE / 8);
        assert_eq!(wc.load(Ordering::Acquire), 0); // no writes yet
        buf.fill(0);
        // read a block with a different tag to cause the cache to write the dirty block out
        block_on(c.copy_bytes(BlockAddress(0x800), 0, &mut buf)).unwrap();
        assert_eq!(rc.load(Ordering::Acquire), 0);
        assert_eq!(wc.load(Ordering::Acquire), PAGE_SIZE / 8);
    }
}
