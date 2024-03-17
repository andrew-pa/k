//! Concurrent hash map implementation.
//!
//! Effectively [DashMap](https://github.com/xacrimon/dashmap) but with a minimal feature set, support for async, and no-std support.

use core::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
};

use crate::tasks::locks::{MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLock};
use alloc::boxed::Box;
use hashbrown::HashMap;

/// A concurrent hash map that can safely be shared between tasks and threads.
///
/// This is a simple implementation based on DashMap.
/// Basically the hash map is split into "shards", which are just smaller synchronous hash maps.
/// Each shard has a [RwLock] and keys are assigned by their hash.
pub struct CHashMap<K, V, S = hashbrown::hash_map::DefaultHashBuilder> {
    /// number of bits to shift down a hash to get the shard index
    shard_index_shift: usize,
    /// the map shards that can be individually locked
    shards: Box<[RwLock<HashMap<K, V, S>>]>,
    /// the hasher builder
    hasher: S,
}

/// An immutable reference to a value in a map. When dropped, the lock is released.
pub type Ref<'a, K, V, S> = MappedRwLockReadGuard<'a, HashMap<K, V, S>, V>;
/// A mutable reference to a value in a map. When dropped, the lock is released.
pub type RefMut<'a, K, V, S> = MappedRwLockWriteGuard<'a, HashMap<K, V, S>, V>;

/// The default number of shards in a map.
///
/// This value is set to four times the number of threads in DashMap, but since each task could
/// potentially lock a shard, it is difficult to say exactly what the best value is here.
/// Effectively it should be something like:
/// `<number of hardware threads> * <expected number of tasks per thread> * 4`.
pub const DEFAULT_SHARD_COUNT: usize = 128;

impl<K, V, S> Default for CHashMap<K, V, S>
where
    K: Eq + Hash,
    S: Default + BuildHasher + Clone,
{
    fn default() -> Self {
        Self::with_hasher_and_shard_count(Default::default(), DEFAULT_SHARD_COUNT)
    }
}

impl<K: Hash + Eq, V, S: BuildHasher + Clone> CHashMap<K, V, S> {
    pub fn with_hasher_and_shard_count(hasher: S, shard_count: usize) -> Self {
        assert!(shard_count > 0);
        assert!(shard_count.is_power_of_two());

        let shard_index_shift = (usize::BITS - shard_count.trailing_zeros()) as usize;

        let shards = (0..shard_count)
            .map(|_| RwLock::new(HashMap::with_capacity_and_hasher(0, hasher.clone())))
            .collect();

        Self {
            shard_index_shift,
            shards,
            hasher,
        }
    }

    /// Get an immutable (and possibly shared) reference to a value in the map by key.
    pub async fn get<Q>(&self, key: &Q) -> Option<Ref<K, V, S>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        let shard_index = self.shard_index_for_hash(hash);
        let shard = unsafe { self.shards.get_unchecked(shard_index) }
            .read()
            .await;
        shard.maybe_map(|m| m.get(key))
    }

    /// Get an mutable (and exclusive) reference to a value in the map by key.
    pub async fn get_mut<Q>(&self, key: &Q) -> Option<RefMut<K, V, S>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        let shard_index = self.shard_index_for_hash(hash);
        let shard = unsafe { self.shards.get_unchecked(shard_index) }
            .write()
            .await;
        shard.maybe_map(|m| m.get_mut(key))
    }

    /// Associates a key with a value in the map.
    /// Returns the old value associated with the key if there was one.
    pub async fn insert(&self, k: K, v: V) -> Option<V> {
        let hash = self.hasher.hash_one(&k);
        let shard_index = self.shard_index_for_hash(hash);
        let mut shard = unsafe { self.shards.get_unchecked(shard_index) }
            .write()
            .await;
        shard.insert(k, v)
    }

    /// Remove a value by its associated key from the map.
    /// Returns the value associated with the key.
    pub async fn remove<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(k);
        let shard_index = self.shard_index_for_hash(hash);
        let mut shard = unsafe { self.shards.get_unchecked(shard_index) }
            .write()
            .await;
        shard.remove(k)
    }

    /// Get an immutable (and possibly shared) reference to a value in the map by key.
    pub fn get_blocking<Q>(&self, key: &Q) -> Option<Ref<K, V, S>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        let shard_index = self.shard_index_for_hash(hash);
        let shard = unsafe { self.shards.get_unchecked(shard_index) }.read_blocking();
        shard.maybe_map(|m| m.get(key))
    }

    /// Get an mutable (and exclusive) reference to a value in the map by key.
    pub fn get_mut_blocking<Q>(&self, key: &Q) -> Option<RefMut<K, V, S>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        let shard_index = self.shard_index_for_hash(hash);
        let shard = unsafe { self.shards.get_unchecked(shard_index) }.write_blocking();
        shard.maybe_map(|m| m.get_mut(key))
    }

    /// Associates a key with a value in the map.
    /// Returns the old value associated with the key if there was one.
    pub fn insert_blocking(&self, k: K, v: V) -> Option<V> {
        let hash = self.hasher.hash_one(&k);
        let shard_index = self.shard_index_for_hash(hash);
        let mut shard = unsafe { self.shards.get_unchecked(shard_index) }.write_blocking();
        shard.insert(k, v)
    }

    /// Remove a value by its associated key from the map.
    /// Returns the value associated with the key.
    pub fn remove_blocking<Q>(&self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(k);
        let shard_index = self.shard_index_for_hash(hash);
        let mut shard = unsafe { self.shards.get_unchecked(shard_index) }.write_blocking();
        shard.remove(k)
    }

    /// Compute the index of the shard responsible for storing the key with this hash.
    fn shard_index_for_hash(&self, hash: u64) -> usize {
        // DashMap leaves the high 7 bits for the HashBrown SIMD tag.
        ((hash << 7) >> self.shard_index_shift) as usize
    }
}

/// A concurrent hash map that uses an autoincrementing counter as a key.
// TODO: no hash hasher here?
pub struct CHashMapAuto<K, V, S = hashbrown::hash_map::DefaultHashBuilder> {
    /// The underlying map.
    map: CHashMap<K, V, S>,
    /// The next key that will be issued.
    next_key: AtomicUsize,
}

impl<K, V, S> Deref for CHashMapAuto<K, V, S> {
    type Target = CHashMap<K, V, S>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<K, V, S> DerefMut for CHashMapAuto<K, V, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl<K: From<usize> + Hash + Eq + Copy, V, S: BuildHasher + Clone> CHashMapAuto<K, V, S> {
    /// Add a value to the map, automatically assigning it the next key.
    /// Returns the key that maps to the value.
    pub async fn insert(&self, v: V) -> K {
        let k = self.reserve_key();
        self.map.insert(k, v).await.expect("keys are unique");
        k
    }

    /// Reserve a key so that a value can be inserted into the map later.
    pub fn reserve_key(&self) -> K {
        K::from(
            self.next_key
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::block_on;

    #[test_case]
    fn insert_and_retrieve() {
        block_on(async {
            let map: CHashMap<&str, &str, hashbrown::hash_map::DefaultHashBuilder> =
                CHashMap::default();
            assert!(map.insert("test1", "test value 1").await.is_none());
            assert!(map.insert("BLUB", "test value 2").await.is_none());
            {
                let t1 = map.get("test1").await;
                assert!(t1.is_some_and(|t| *t == "test value 1"));
            }
            {
                let blub = map.get("BLUB").await;
                if let Some(b) = blub.as_ref() {
                    log::debug!("blub is {}", **b);
                } else {
                    log::debug!("blub is none");
                }
                assert!(blub.is_some_and(|t| *t == "test value 2"));
            }
            {
                assert!(map.get("not in the map").await.is_none());
            }
        });
    }

    #[test_case]
    fn insert_and_remove() {
        log::trace!("test starts");
        block_on(async {
            log::trace!("test task starts");
            let map: CHashMap<&str, &str, hashbrown::hash_map::DefaultHashBuilder> =
                CHashMap::default();
            log::trace!("map allocated");
            assert!(map.insert("test1", "test value 1").await.is_none());
            assert!(map.insert("BLUB", "test value 2").await.is_none());
            log::trace!("post insert");
            {
                let t1 = map.remove("test1").await;
                assert!(t1.is_some_and(|t| t == "test value 1"));
            }
            log::trace!("after first remove");
            {
                let blub = map.remove("BLUB").await;
                if let Some(b) = blub.as_ref() {
                    log::debug!("blub is {}", *b);
                } else {
                    log::debug!("blub is none");
                }
                assert!(blub.is_some_and(|t| t == "test value 2"));
                log::trace!("post assert 278");
            }
            {
                assert!(map.get("test1").await.is_none());
                assert!(map.get("BLUB").await.is_none());
            }
            log::trace!("end");
        });
    }
}
