//! First-result-wins cuckoo hashing with unaligned buckets.
//! 
//! See https://arxiv.org/pdf/1707.06855 for analysis of unaligned buckets.
//! 
//! See https://cbg.netlify.app/publication/research_cuckoo_lsa/ for a better algorithm than Random Walk.
//! See https://cbg.netlify.app/publication/research_cuckoo_cbg/ for a claimed good implementation.
//! 
//! Unfortunately, both of those algorithms require adding a few bits to the metadata table, which
//! we don't want to do, since we want to maintain compatibility with SwissTable layout.
//! 
//! https://www.cs.cmu.edu/~dga/papers/memc3-nsdi2013.pdf <-- this paper uses hash(key)^hash(tag) as
//! the secondary key.
//! 
//! https://news.ycombinator.com/item?id=14290055 <-- some discussion from Frank McSherry and Paul Khuong on hash tables.
//! 
//! https://www.cs.princeton.edu/~mfreed/docs/cuckoo-eurosys14.pdf <-- follow-up on libcuckoo/MemC3. They explain why they use BFS rather than DFS. Some is irrelevant (critical section length) but some is relevant: BFS offers better memory level parallelism via prefetching.

use std::{alloc::Layout, ptr::NonNull};

use crate::TRACK_PROBE_LENGTH;
use crate::control::{Group, Tag, TagSliceExt as _};
use crate::u64_fold_hash_fast::{self, fold_hash_fast};
use crate::uunwrap::UUnwrap;
use crate::dropper::Dropper;

pub struct HashTable<V> {
    // Mask to get an index from a hash value. The value is one less than the
    // number of buckets in the table.
    bucket_mask: usize,

    // [Padding], T_n, ..., T1, T0, C0, C1, ...
    //                              ^ points here
    ctrl: NonNull<u8>,

    // Number of elements in the table, only really used by len()
    items: usize,

    // Seed for the hash function
    seed: u64,

    marker: std::marker::PhantomData<V>,
    rng: fastrand::Rng,
    total_probe_length: usize,
    total_insert_probe_length: usize,
    max_insert_probe_length: usize,
    dropper: Dropper,
}

impl<V> HashTable<V> {
    pub fn with_capacity(capacity: usize) -> Self {
        // Calculate sizes
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7).next_power_of_two();
        let bucket_size = std::mem::size_of::<(u64, V)>();
        let align = std::mem::align_of::<(u64, V)>().max(Group::WIDTH);
        let ctrl_offset = (bucket_size * num_buckets).next_multiple_of(align);
        let size = ctrl_offset + num_buckets + Group::WIDTH;
        let layout = Layout::from_size_align(size, align).uunwrap();
        // Allocate
        let alloc = unsafe { std::alloc::alloc(layout) };
        // Write control
        let ctrl = unsafe { NonNull::new_unchecked(alloc.add(ctrl_offset)) };
        let ctrl_slice = unsafe { std::slice::from_raw_parts_mut(ctrl.as_ptr() as *mut Tag, num_buckets + Group::WIDTH) };
        ctrl_slice.fill_empty();
        // dbg!(num_buckets, bucket_size, align, ctrl_offset, size, layout, alloc, ctrl);
        let seed = fastrand::Rng::with_seed(123).u64(..);
        let bucket_mask = num_buckets - 1;

        Self {
            bucket_mask,
            ctrl,
            items: 0,
            seed,
            marker: std::marker::PhantomData,
            rng: fastrand::Rng::with_seed(123),
            total_probe_length: 0,
            total_insert_probe_length: 0,
            max_insert_probe_length: 0,
            dropper: Dropper { alloc, layout },
        }
    }

    pub fn print_stats(&self) {
        let items = self.items as f64;
        println!("  avg_probe_length: {}", self.total_probe_length as f64 / items);
        println!("  avg_insert_probe_length: {}", self.total_insert_probe_length as f64 / items);
        println!("  max_insert_probe_length: {}", self.max_insert_probe_length);
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.items
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, usize) {
        let hash0 = fold_hash_fast(key, self.seed);
        let hash1 = hash0.rotate_left(32);
        let tag_hash = Tag::full(hash0);

        // Probe first group for a match.
        let pos0 = hash0 as usize & self.bucket_mask;
        let group0 = unsafe { Group::load(self.ctrl(pos0)) };

        for bit in group0.match_tag(tag_hash) {
            let index = (pos0 + bit) & self.bucket_mask;

            let bucket = unsafe { self.bucket(index) };

            if unsafe { (*bucket).0 } == key {
                unsafe { (*bucket).1 = value };
                return (false, index);
            }
        }

        // Probe second group for a match.
        let pos1 = hash1 as usize & self.bucket_mask;
        let group1 = unsafe { Group::load(self.ctrl(pos1)) };

        for bit in group1.match_tag(tag_hash) {
            let index = (pos1 + bit) & self.bucket_mask;

            let bucket = unsafe { self.bucket(index) };

            if unsafe { (*bucket).0 } == key {
                unsafe { (*bucket).1 = value };
                return (false, index);
            }
        }

        let mut insert_probe_length = 1;

        // No match. Now check first group for an empty slot.
        if let Some(insert_slot) = group0.match_empty().lowest_set_bit() {
            let insert_slot = (pos0 + insert_slot) & self.bucket_mask;
            unsafe { 
                self.set_ctrl(insert_slot, tag_hash);
                self.bucket(insert_slot).write((key, value));
                self.items += 1;
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += 1;
                    self.total_insert_probe_length += insert_probe_length;
                    self.max_insert_probe_length = self.max_insert_probe_length.max(insert_probe_length);
                }
                return (true, insert_slot);
            }
        }

        // key is going to get inserted in the second location.
        if TRACK_PROBE_LENGTH {
            self.total_probe_length += 2;
        }

        // BFS Cuckoo loop adapted for unaligned buckets.
        // Each key can be in two different windows, so we explore both alternatives.
        // This is similar to aligned_cuckoo_table.rs but adapted for two alternatives per key.
        use std::mem::MaybeUninit;

        const N: usize = Group::WIDTH;
        const BFS_MAX_LEN: usize = 2 * (1 + 2*N + 2*N*N + 2*N*N*N);

        let mut pos0 = pos0;
        let mut pos1 = pos1;
        let mut group0 = group0;
        let mut group1 = group1;

        let mut bfs_queue = [MaybeUninit::<usize>::uninit(); BFS_MAX_LEN];
        bfs_queue[0].write(pos0);
        bfs_queue[1].write(pos1);
        let mut bfs_read_pos = 0;
        let (mut path_index, mut bucket_index) = loop {
            if let Some(empty_pos) = group0.match_empty().lowest_set_bit() {
                break (bfs_read_pos + 0, (pos0 + empty_pos) & self.bucket_mask);
            }
            if let Some(empty_pos) = group1.match_empty().lowest_set_bit() {
                break (bfs_read_pos + 1, (pos1 + empty_pos) & self.bucket_mask);
            }

            let bfs_write_pos = bfs_read_pos * 2 * N + 2;
            if bfs_write_pos + 2 * 2 * N <= BFS_MAX_LEN {
                // For each bucket in current two windows
                for i in 0..N {
                    // Current window 0
                    let bucket_idx = (pos0 + i) & self.bucket_mask;
                    let key = unsafe { (*self.bucket(bucket_idx)).0 };
                    let rehash = fold_hash_fast(key, self.seed);
                    let alt_pos0 = rehash as usize & self.bucket_mask;
                    let alt_pos1 = rehash.rotate_left(32) as usize & self.bucket_mask;

                    unsafe {
                        *bfs_queue.get_unchecked_mut(bfs_write_pos + i * 2).write(alt_pos0);
                        *bfs_queue.get_unchecked_mut(bfs_write_pos + i * 2 + 1).write(alt_pos1);
                    }
                }
                for i in 0..N {
                    // Current window 1
                    let bucket_idx = (pos1 + i) & self.bucket_mask;
                    let key = unsafe { (*self.bucket(bucket_idx)).0 };
                    let rehash = fold_hash_fast(key, self.seed);
                    let alt_pos0 = rehash as usize & self.bucket_mask;
                    let alt_pos1 = rehash.rotate_left(32) as usize & self.bucket_mask;

                    unsafe {
                        *bfs_queue.get_unchecked_mut(bfs_write_pos + 2 * N + i * 2).write(alt_pos0);
                        *bfs_queue.get_unchecked_mut(bfs_write_pos + 2 * N + i * 2 + 1).write(alt_pos1);
                    }
                }
            }

            bfs_read_pos += 2;

            if bfs_read_pos + 2 > BFS_MAX_LEN {
                panic!("Failed to insert into cuckoo table; need to rehash");
            }
            pos0 = unsafe { bfs_queue[bfs_read_pos + 0].assume_init() };
            pos1 = unsafe { bfs_queue[bfs_read_pos + 1].assume_init() };
            group0 = unsafe { Group::load(self.ctrl(pos0)) };
            group1 = unsafe { Group::load(self.ctrl(pos1)) };
        };

        // Backtrack from the empty slot to the root, moving keys along the path
        while path_index >= 2 {
            let parent_path_index = (path_index - 2) / (2 * N);
            let parent_bucket_offset = (path_index - 2) % (2 * N);

            // Each level stores 2*N*2 = 4*N positions
            // Layout: window0_bucket0_pos0, window0_bucket0_pos1, window0_bucket1_pos0, window0_bucket1_pos1, ...,
            //         window1_bucket0_pos0, window1_bucket0_pos1, window1_bucket1_pos0, window1_bucket1_pos1, ...
            let parent_window_index = parent_bucket_offset / (2 * N);  // 0 or 1
            let parent_bucket_in_window = (parent_bucket_offset % (2 * N)) / 2;
            let parent_alt_index = parent_bucket_offset % 2;

            let parent_pos = unsafe { bfs_queue.get_unchecked(parent_path_index + parent_window_index).assume_init() };
            let parent_bucket_index = (parent_pos + parent_bucket_in_window) & self.bucket_mask;

            // Move from parent to child
            unsafe {
                let parent_kv = self.bucket(parent_bucket_index).read();
                let parent_tag = *self.ctrl(parent_bucket_index);
                self.bucket(bucket_index).write(parent_kv);
                self.set_ctrl(bucket_index, parent_tag);
            }

            bucket_index = parent_bucket_index;
            path_index = parent_path_index + parent_window_index;
        }

        unsafe {
            self.bucket(bucket_index).write((key, value));
            self.set_ctrl(bucket_index, tag_hash);
            self.items += 1;
            insert_probe_length += path_index + 1;
            if TRACK_PROBE_LENGTH {
                self.total_insert_probe_length += insert_probe_length;
                self.max_insert_probe_length = self.max_insert_probe_length.max(insert_probe_length);
            }
            return (true, bucket_index);
        }
    }

    #[inline(always)]
    pub unsafe fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, index) = self.insert(key, value);
        if inserted {
            unsafe {
                self.set_ctrl(index, Tag::EMPTY);
            }
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let mut hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);

        let mut is_second_group = false;

        // First group
        loop {
            let pos = hash64 as usize & self.bucket_mask;
            let group = unsafe { Group::load(self.ctrl(pos)) };
            for bit in group.match_tag(tag_hash) {
                let index = (pos + bit) & self.bucket_mask;

                let bucket = unsafe { self.bucket(index) };

                if unsafe { (*bucket).0 } == key {
                    return Some(unsafe { &(*bucket).1 });
                }
            }
            const ALLOW_EARLY_RETURN: bool = true;
            if is_second_group || (ALLOW_EARLY_RETURN && group.match_empty().any_bit_set()) {
                return None;
            }
            hash64 = hash64.rotate_left(32);
            is_second_group = true;
        }

        // // Second group
        // let pos1 = hash64.rotate_left(32) as usize & self.bucket_mask;
        // let group1 = unsafe { Group::load(self.ctrl(pos1)) };
        // for bit in group1.match_tag(tag_hash) {
        //     let index = (pos1 + bit) & self.bucket_mask;

        //     let bucket = unsafe { self.bucket(index) };

        //     if unsafe { (*bucket).0 } == key {
        //         return Some(index);
        //     }
        // }
        // None
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        let mut hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let mut probe_count = 0;

        // First group
        loop {
            probe_count += 1;
            let pos = hash64 as usize & self.bucket_mask;
            let group = unsafe { Group::load(self.ctrl(pos)) };

            for bit in group.match_tag(tag_hash) {
                let index = (pos + bit) & self.bucket_mask;
                let bucket = unsafe { self.bucket(index) };
                if unsafe { (*bucket).0 } == key {
                    return (probe_count, true); // Key found
                }
            }

            if group.match_empty().any_bit_set() {
                return (probe_count, false); // Empty slot found, key absent
            }

            if probe_count >= 2 {
                return (probe_count, false); // After checking both groups, key absent
            }

            hash64 = hash64.rotate_left(32);
        }
    }

    #[inline(always)]
    pub unsafe fn erase_index(&mut self, index: usize) {
        let index_before = index.wrapping_sub(Group::WIDTH) & self.bucket_mask;
        let empty_before = Group::load(self.ctrl(index_before)).match_empty();
        let empty_after = Group::load(self.ctrl(index)).match_empty();
        let ctrl = if empty_before.leading_zeros() + empty_after.trailing_zeros() >= Group::WIDTH {
            Tag::DELETED
        } else {
            Tag::EMPTY
        };
        self.set_ctrl(index, ctrl);
        self.items -= 1;

    }

    #[inline(always)]
    unsafe fn ctrl(&self, index: usize) -> *mut Tag {
        self.ctrl.as_ptr().add(index).cast()
    }

    #[inline(always)]
    unsafe fn bucket(&self, index: usize) -> *mut (u64, V) {
        let data_end: *mut (u64, V) = self.ctrl.as_ptr().cast();
        data_end.sub(index + 1)
    }

    #[inline(always)]
    unsafe fn set_ctrl(&self, index: usize, tag: Tag) {
        let other_index = (index.wrapping_sub(Group::WIDTH) & self.bucket_mask) + Group::WIDTH;
        *self.ctrl(index) = tag;
        *self.ctrl(other_index) = tag;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_basic_insert_and_get() {
        let mut table = HashTable::with_capacity(16);

        // Test basic insertion
        let (inserted, _) = table.insert(42, 100);
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Test retrieval
        assert_eq!(table.get(&42), Some(&100));
        assert_eq!(table.get(&999), None);
    }

    #[test]
    fn test_update_existing() {
        let mut table = HashTable::with_capacity(16);

        // Insert initial value
        let (inserted, _) = table.insert(123, 456);
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Update with new value
        let (inserted, _) = table.insert(123, 789);
        assert!(!inserted); // Should be false since key already existed
        assert_eq!(table.len(), 1); // Length should remain the same

        // Verify updated value
        assert_eq!(table.get(&123), Some(&789));
    }

    #[test]
    fn test_multiple_insertions() {
        let mut table = HashTable::with_capacity(64);

        // Insert multiple values
        for i in 1..=20 {
            let (inserted, _) = table.insert(i, i * 10);
            assert!(inserted);
        }

        assert_eq!(table.len(), 20);

        // Verify all values
        for i in 1..=20 {
            assert_eq!(table.get(&i), Some(&(i * 10)));
        }
    }

    #[test]
    fn test_cross_check_with_std_hashmap_small() {
        let mut cuckoo_table = HashTable::with_capacity(32);
        let mut std_map = HashMap::new();

        let keys = [1, 5, 10, 15, 20, 25, 30, 35];

        // Insert same data into both
        for &key in &keys {
            let value = key * 2;
            cuckoo_table.insert(key, value);
            std_map.insert(key, value);
        }

        // Verify both have same length
        assert_eq!(cuckoo_table.len(), std_map.len());

        // Verify all lookups match
        for &key in &keys {
            assert_eq!(cuckoo_table.get(&key).copied(), std_map.get(&key).copied());
        }

        // Test non-existent keys
        for &key in &[2, 7, 12, 99] {
            assert_eq!(cuckoo_table.get(&key), None);
            assert_eq!(std_map.get(&key), None);
        }
    }

    #[test]
    fn test_randomized_small() {
        let mut rng = fastrand::Rng::with_seed(12345);
        let mut cuckoo_table = HashTable::with_capacity(128);
        let mut std_map = HashMap::new();

        // Random insertions
        for _ in 0..50 {
            let key = rng.u64(1..1000); // Avoid key 0 for simplicity
            let value = rng.u64(..);

            let cuckoo_result = cuckoo_table.insert(key, value);
            let std_existed = std_map.insert(key, value).is_some();

            // Check insertion result consistency
            assert_eq!(cuckoo_result.0, !std_existed);
        }

        // Verify lengths match
        assert_eq!(cuckoo_table.len(), std_map.len());

        // Verify all lookups match
        for &key in std_map.keys() {
            assert_eq!(cuckoo_table.get(&key).copied(), std_map.get(&key).copied());
        }
    }

    #[test]
    fn test_randomized_medium() {
        let mut rng = fastrand::Rng::with_seed(67890);
        let mut cuckoo_table = HashTable::with_capacity(512);
        let mut std_map = HashMap::new();

        // Random insertions and updates
        for _ in 0..200 {
            let key = rng.u64(1..500);
            let value = rng.u64(..);

            let cuckoo_result = cuckoo_table.insert(key, value);
            let std_existed = std_map.insert(key, value).is_some();

            assert_eq!(cuckoo_result.0, !std_existed);
        }

        assert_eq!(cuckoo_table.len(), std_map.len());

        // Random lookups - both existing and non-existing keys
        for _ in 0..100 {
            let key = rng.u64(1..1000);
            assert_eq!(cuckoo_table.get(&key).copied(), std_map.get(&key).copied());
        }
    }

    #[test]
    fn test_collision_handling() {
        let mut table = HashTable::with_capacity(8); // Small table to force collisions

        // Insert many values that may hash to similar locations
        let test_keys = [
            0x1000_0000_0000_0001,
            0x2000_0000_0000_0002,
            0x3000_0000_0000_0003,
            0x4000_0000_0000_0004,
            0x5000_0000_0000_0005,
        ];

        for &key in &test_keys {
            let (inserted, _) = table.insert(key, key);
            assert!(inserted);
        }

        // Verify all keys can be retrieved
        for &key in &test_keys {
            assert_eq!(table.get(&key), Some(&key));
        }
    }

    #[test]
    fn test_capacity_stress() {
        let mut cuckoo_table = HashTable::with_capacity(64);
        let mut std_map = HashMap::new();
        let mut rng = fastrand::Rng::with_seed(42);

        // Fill to reasonable capacity (cuckoo hashing typically works well up to ~90% load)
        let num_items = 45; // About 70% of capacity

        for _ in 0..num_items {
            let key = loop {
                let k = rng.u64(1..u64::MAX);
                if !std_map.contains_key(&k) { break k; }
            };
            let value = rng.u64(..);

            cuckoo_table.insert(key, value);
            std_map.insert(key, value);
        }

        assert_eq!(cuckoo_table.len(), std_map.len());
        assert_eq!(cuckoo_table.len(), num_items);

        // Verify all insertions
        for (&key, &expected_value) in &std_map {
            assert_eq!(cuckoo_table.get(&key), Some(&expected_value));
        }
    }

    #[test]
    fn test_update_pattern() {
        let mut cuckoo_table = HashTable::with_capacity(32);
        let mut std_map = HashMap::new();

        // Insert initial values
        for i in 1..=10 {
            cuckoo_table.insert(i, i);
            std_map.insert(i, i);
        }

        // Update all values multiple times
        for round in 1..=3 {
            for i in 1..=10 {
                let new_value = i * 100 * round;
                let (cuckoo_inserted, _) = cuckoo_table.insert(i, new_value);
                let std_existed = std_map.insert(i, new_value).is_some();

                assert!(!cuckoo_inserted); // Should be update, not insert
                assert!(std_existed); // Should be update, not insert
            }

            // Verify all updates
            for i in 1..=10 {
                let expected = i * 100 * round;
                assert_eq!(cuckoo_table.get(&i), Some(&expected));
                assert_eq!(std_map.get(&i), Some(&expected));
            }
        }
    }

    #[test]
    fn test_mixed_operations_randomized() {
        let mut rng = fastrand::Rng::with_seed(13579);
        let mut cuckoo_table = HashTable::with_capacity(256);
        let mut std_map = HashMap::new();

        // Mixed operations: inserts, updates, lookups
        for _ in 0..300 {
            let operation = rng.u32(0..3);

            match operation {
                0 => {
                    // Insert/Update
                    let key = rng.u64(1..200);
                    let value = rng.u64(..);

                    let cuckoo_result = cuckoo_table.insert(key, value);
                    let std_existed = std_map.insert(key, value).is_some();
                    assert_eq!(cuckoo_result.0, !std_existed);
                }
                1 => {
                    // Lookup existing key
                    if let Some(&key) = std_map.keys().next() {
                        assert_eq!(cuckoo_table.get(&key).copied(), std_map.get(&key).copied());
                    }
                }
                2 => {
                    // Lookup random key (may or may not exist)
                    let key = rng.u64(1..300);
                    assert_eq!(cuckoo_table.get(&key).copied(), std_map.get(&key).copied());
                }
                _ => unreachable!()
            }
        }

        // Final consistency check
        assert_eq!(cuckoo_table.len(), std_map.len());

        for (&key, &value) in &std_map {
            assert_eq!(cuckoo_table.get(&key), Some(&value));
        }
    }

    #[test]
    fn test_high_load_factor_insertion_debug() {
        // This test debugs the benchmark issue at high load factors
        let capacity = 32768; // Large capacity similar to benchmark
        let mut table = HashTable::with_capacity(capacity);
        let n = capacity * 3 / 4; // 75% load factor like in the benchmark

        println!("Testing insertion of {} keys into capacity {}", n, capacity);

        let mut failed_keys = Vec::new();
        for i in 0..n {
            let key = i as u64;
            let (inserted, _) = table.insert(key, key);
            if !inserted {
                // This means key already existed, which shouldn't happen with sequential keys
                println!("WARNING: Key {} was already in table!", key);
            }
        }

        // Check that all keys can be found
        for i in 0..n {
            let key = i as u64;
            if table.get(&key).is_none() {
                failed_keys.push(key);
            }
        }

        if !failed_keys.is_empty() {
            println!("FAILED TO INSERT {} keys: {:?}", failed_keys.len(), &failed_keys[..10.min(failed_keys.len())]);
        }

        println!("Table length: {}, Expected: {}", table.len(), n);
        assert_eq!(failed_keys.len(), 0, "Some keys failed to insert at 75% load factor");
    }

    #[test]
    #[should_panic]
    fn test_very_high_load_factor() {
        // This test should fail due to cuckoo hashing limitations
        let mut table = HashTable::with_capacity(16);

        // Try to insert way more than capacity (should fail)
        for i in 0..50 {
            let (inserted, _) = table.insert(i, i);
            println!("Inserted key {}: {}", i, inserted);
        }
    }
}