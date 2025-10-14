//! A cuckoo hash table with 2 choices of group, each with 8-16 buckets per group.

use std::hint::{black_box, likely};
use std::mem::MaybeUninit;
use std::{alloc::Layout, ptr::NonNull};

use crate::TRACK_PROBE_LENGTH;
use crate::control::{Group, Tag, TagSliceExt as _};
use crate::u64_fold_hash_fast::{self, fold_hash_fast};
use crate::uunwrap::UUnwrap;

pub struct HashTable<V: Copy> {
    aligned_bucket_mask: usize,

    // [Padding], T_n, ..., T1, T0, C0, C1, ...
    //                              ^ points here
    ctrl: NonNull<u8>,

    // Number of elements in the table, only really used by len()
    items_until_growth: usize,

    // Seed for the hash function
    seed: u64,

    marker: std::marker::PhantomData<V>,

    total_probe_length: usize,
    total_insert_probe_length: usize,
    max_insert_probe_length: usize,
}

impl<V: Copy> HashTable<V> {
    /// Create a new hash table with a small initial capacity that will grow as needed.
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        // Calculate sizes
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7).next_power_of_two();
        let bucket_size = std::mem::size_of::<(u64, V)>();
        let align = std::mem::align_of::<(u64, V)>().max(Group::WIDTH);
        let ctrl_offset = (bucket_size * num_buckets).next_multiple_of(align);
        let size = ctrl_offset + num_buckets;
        let layout = Layout::from_size_align(size, align).uunwrap();
        // Allocate
        let alloc = unsafe { std::alloc::alloc(layout) };
        // Write control
        let ctrl = unsafe { NonNull::new_unchecked(alloc.add(ctrl_offset)) };
        let ctrl_slice =
            unsafe { std::slice::from_raw_parts_mut(ctrl.as_ptr() as *mut Tag, num_buckets) };
        ctrl_slice.fill_empty();
        // dbg!(num_buckets, bucket_size, align, ctrl_offset, size, layout, alloc, ctrl);
        let seed = fastrand::Rng::with_seed(123).u64(..);
        let bucket_mask = num_buckets - 1;
        let aligned_bucket_mask = num_buckets - Group::WIDTH;
        let items_until_growth = (num_buckets * 7) / 8;
        // println!("num_buckets = {}, items_until_growth = {}", num_buckets, items_until_growth);

        Self {
            aligned_bucket_mask,
            ctrl,
            items_until_growth,
            seed,
            marker: std::marker::PhantomData,
            total_probe_length: 0,
            total_insert_probe_length: 0,
            max_insert_probe_length: 0,
        }
    }

    #[inline(always)]
    unsafe fn dealloc(ctrl: NonNull<u8>, num_buckets: usize) {
        let align = std::mem::align_of::<(u64, V)>().max(Group::WIDTH);
        let bucket_size = std::mem::size_of::<(u64, V)>();
        let ctrl_offset = (bucket_size * num_buckets).next_multiple_of(align);
        let size = ctrl_offset + num_buckets;
        let layout = Layout::from_size_align(size, align).uunwrap();
        unsafe { std::alloc::dealloc(ctrl.as_ptr().sub(ctrl_offset), layout) };
    }

    #[inline(always)]
    fn item_capacity(aligned_bucket_mask: usize) -> usize {
        ((aligned_bucket_mask + Group::WIDTH) * 7) / 8
    }

    // Item capacity when doubling:
    //    (((new_aligned_bucket_mask + Group::WIDTH) * 7) / 8) - (((old_aligned_bucket_mask + Group::WIDTH) * 7) / 8)
    // =  (new_aligned_bucket_mask - old_aligned_bucket_mask) * 7 / 8

    /// Safety: caller promises that there have been no tombstones in the table.
    #[inline(always)]
    pub unsafe fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, index, _) = self.insert(key, value);
        if inserted {
            unsafe {
                self.set_ctrl(index, Tag::EMPTY);
            }
            self.items_until_growth += 1;
        }
    }

    #[inline(always)]
    pub fn avg_probe_length(&self) -> f64 {
        self.total_probe_length as f64 / self.len() as f64
    }

    #[inline(always)]
    pub fn print_stats(&self) {
        let items = self.len() as f64;
        println!(
            "  avg_probe_length: {}",
            self.total_probe_length as f64 / items
        );
        println!(
            "  avg_insert_probe_length: {}",
            self.total_insert_probe_length as f64 / items
        );
        println!(
            "  max_insert_probe_length: {}",
            self.max_insert_probe_length
        );
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        Self::item_capacity(self.aligned_bucket_mask) - self.items_until_growth
    }

    /// Returns the number of buckets in the table.
    #[inline(always)]
    pub fn num_buckets(&self) -> usize {
        self.aligned_bucket_mask + Group::WIDTH
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, usize, usize) {
        // println!("insert");
        let hash0 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash0);
        let hash1 = hash0 ^ scramble_tag(tag_hash);
        let mut insertion_probe_length = 1; // Start with 1 probe

        const EARLY_RETURN: bool = true;

        let (bucket, index) = 'hit: loop {
            let bucket_index = 'search_empty: loop {
                let pos0 = hash0 as usize & self.aligned_bucket_mask;
                let group0 = unsafe { Group::load(self.ctrl(pos0)) };

                // Probe first group for a match.
                for bit in group0.match_tag(tag_hash) {
                    let index = pos0 + bit;

                    let bucket = unsafe { self.bucket(index) };

                    if unsafe { (*bucket).0 } == key {
                        break 'hit (bucket, index);
                    }
                }

                // Probe second group for a match.
                insertion_probe_length = 2; // If we reach here, we've probed 2 groups
                let pos1 = hash1 as usize & self.aligned_bucket_mask;
                let group1 = unsafe { Group::load(self.ctrl(pos1)) };

                for bit in group1.match_tag(tag_hash) {
                    let index = pos1 + bit;

                    let bucket = unsafe { self.bucket(index) };

                    if unsafe { (*bucket).0 } == key {
                        break 'hit (bucket, index);
                    }
                }

                // Now search for (a path to) an empty slot.
                if let Some(insert_slot) = group0.match_empty().lowest_set_bit() {
                    let insert_slot = pos0 + insert_slot;
                    insertion_probe_length = 1; // Found in first group
                    // println!("slot 0 empty");
                    break 'search_empty insert_slot;
                }
                // TODO: can't do this one...
                if let Some(insert_slot) = group1.match_empty().lowest_set_bit() {
                    let insert_slot = pos1 + insert_slot;
                    insertion_probe_length = 2; // Found in second group
                    // println!("slot 1 empty");
                    break 'search_empty insert_slot;
                }

                // Cuckoo loop. BFS queue maintains group indexes to visit.
                //
                // We search two complete N-ary trees, where N=Group::WIDTH. We search up to depth D=3, i.e.
                // 2 groups at the first level, 2*N, 2*N^2, 2*N^3.
                //
                // The parent of node at index `i` is at index `(i-2)/N`. Inversely, the first child of
                // node `j` is at index `j*N+2`.
                const N: usize = Group::WIDTH;
                const BFS_MAX_LEN: usize = 2 * (1 + N + N * N + N * N * N + N * N * N * N + N * N * N * N * N);

                #[derive(Clone, Copy)]
                struct PathNode {
                    bucket: usize,
                    parent_index_in_bfs_queue: u16,
                    slot_index_in_parent: u8,
                }

                let mut best_cost = isize::MAX;
                let mut best_path_terminator = PathNode {
                    bucket: 0,
                    parent_index_in_bfs_queue: u16::MAX,
                    slot_index_in_parent: 0,
                }; // Index into BFS queue.
                let mut best_bucket_index = usize::MAX;

                #[derive(Clone, Copy)]
                struct BFSQueueEntry {
                    cost: isize,
                    path_node: PathNode,
                }

                // TODO: using a HashSet is clearly not optimal. Better would be a bitmask embedded in the underlying array.
                let mut bfs_queue_set = std::collections::HashSet::new();

                let mut bfs_queue = [MaybeUninit::<BFSQueueEntry>::uninit(); BFS_MAX_LEN];
                bfs_queue[0].write(BFSQueueEntry {
                    cost: 0,
                    path_node: PathNode {
                        bucket: pos0,
                        parent_index_in_bfs_queue: 0,
                        slot_index_in_parent: 0,
                    },
                });
                bfs_queue[1].write(BFSQueueEntry {
                    cost: 1,
                    path_node: PathNode {
                        bucket: pos1,
                        parent_index_in_bfs_queue: 0,
                        slot_index_in_parent: 0,
                    },
                });
                let mut bfs_read_pos = 0;
                let mut bfs_write_pos = 2;
                'bfs: loop {
                    if bfs_read_pos >= bfs_write_pos {
                        debug_assert!(best_cost < isize::MAX);
                        break 'bfs;
                    }
                    let entry = unsafe { bfs_queue[bfs_read_pos].assume_init() };
                    let pos = entry.path_node.bucket;
                    let cost = entry.cost;

                    for i in 0..N {
                        let key = unsafe { (*self.bucket(pos + i)).0 };
                        check_key_in_bucket(key, pos, self.aligned_bucket_mask, self.seed);
                        let hash0 = fold_hash_fast(key, self.seed);
                        let tag_hash = Tag::full(hash0);
                        let hash1 = hash0 ^ scramble_tag(tag_hash);
                        let is_first_choice = hash0 as usize & self.aligned_bucket_mask == pos;
                        let is_second_choice = hash1 as usize & self.aligned_bucket_mask == pos;
                        assert!(
                            is_first_choice || is_second_choice,
                            "{hash0:x}, {hash1:x}, {pos:x}, {mask:x}",
                            mask = self.aligned_bucket_mask
                        );
                        let child_cost =
                            cost + std::hint::select_unpredictable(is_first_choice, 1, -1);
                        if child_cost < best_cost {
                            let other_pos = pos
                                ^ (scramble_tag(unsafe { *self.ctrl(pos + i) }) as usize
                                    & self.aligned_bucket_mask);
                            let other_group = unsafe { Group::load(self.ctrl(other_pos)) };

                            let path_node = PathNode {
                                bucket: other_pos,
                                parent_index_in_bfs_queue: bfs_read_pos as u16,
                                slot_index_in_parent: i as u8,
                            };
                            if let Some(empty_pos) = other_group.match_empty().lowest_set_bit() {
                                // println!("bfs candidate found");
                                best_cost = child_cost;
                                best_path_terminator = path_node;
                                best_bucket_index = other_pos + empty_pos;
                            } else {
                                if bfs_write_pos >= BFS_MAX_LEN {
                                    panic!();
                                    // panic!("Failed to insert into cuckoo table; need to rehash, items_until_growth = {}, items = {}, num_buckets = {}", self.items_until_growth, self.len(), self.num_buckets());
                                }

                                if !bfs_queue_set.insert(other_pos) {
                                    continue;
                                }
                                unsafe {
                                    *bfs_queue.get_unchecked_mut(bfs_write_pos).write(
                                        BFSQueueEntry {
                                            cost: child_cost,
                                            path_node: path_node,
                                        },
                                    );
                                    bfs_write_pos += 1;
                                }
                            }
                        }
                    }

                    bfs_read_pos += 1;
                } // 'bfs
                let mut path_node = best_path_terminator;
                let mut bucket_index = best_bucket_index;
                loop {
                    // println!("ascending bfs path");
                    let parent_index_in_bfs_queue = path_node.parent_index_in_bfs_queue;
                    let parent_node =
                        unsafe { bfs_queue[parent_index_in_bfs_queue as usize].assume_init() }
                            .path_node;
                    let parent_bucket_index =
                        parent_node.bucket + path_node.slot_index_in_parent as usize;

                    // Move from parent to child.
                    unsafe {
                        let parent_kv = self.bucket(parent_bucket_index).read();
                        check_key_in_bucket(
                            parent_kv.0,
                            bucket_index & self.aligned_bucket_mask,
                            self.aligned_bucket_mask,
                            self.seed,
                        );
                        self.bucket(bucket_index).write(parent_kv);
                        self.set_ctrl(bucket_index, unsafe { *self.ctrl(parent_bucket_index) });
                    }
                    bucket_index = parent_bucket_index;
                    path_node = parent_node;
                    if parent_index_in_bfs_queue < 2 {
                        break;
                    }
                }
                break 'search_empty bucket_index;
            }; // 'search_empty

            // println!("inserting key slot");
            check_key_in_bucket(
                key,
                bucket_index & self.aligned_bucket_mask,
                self.aligned_bucket_mask,
                self.seed,
            );
            self.items_until_growth -= 1;
            unsafe {
                self.bucket(bucket_index).write((key, value));
                self.set_ctrl(bucket_index, tag_hash);
            }
            // println!("inserted key {} at bucket {} of {}. hash0 = {}, hash1 = {}", key, bucket_index, self.num_buckets(), hash0 as usize & self.aligned_bucket_mask, hash1 as usize & self.aligned_bucket_mask);
            return (true, bucket_index, insertion_probe_length);
        }; // 'hit
        unsafe { (*bucket).1 = value };
        return (false, index, insertion_probe_length);
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let mut hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let mut is_second_group = false;

        // First group
        loop {
            let pos = hash64 as usize & self.aligned_bucket_mask;
            // println!("searching for key at bucket {}", pos);
            let group = unsafe { Group::load(self.ctrl(pos)) };
            for bit in group.match_tag(tag_hash) {
                let index = pos + bit;

                let bucket = unsafe { self.bucket(index) };

                if likely(unsafe { (*bucket).0 } == key) {
                    return Some(unsafe { &(*bucket).1 });
                }
            }
            // We skip early return on empty slots.
            // * early return has ~no impact on find_hit, since we will have found the key anyway.
            // * early return *slows down* in-cache find_miss, perhaps simply from time spent checking
            //   for empty slots.
            // * early return prevents deletions from working.
            //
            // Additionally, given early return is disabled, we can improve probe lengths even further,
            // by doing "less-loaded" cuckoo insertions. We don't do that in this table but instead in
            // a later one.
            const ALLOW_EARLY_RETURN: bool = false;
            if (ALLOW_EARLY_RETURN && likely(group.match_empty().any_bit_set())) || is_second_group
            {
                return None;
            }
            let tag64 = scramble_tag(tag_hash);
            hash64 = hash64 ^ tag64;
            is_second_group = true;
        }
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        let mut hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let mut probe_count = 0;

        // First group
        loop {
            probe_count += 1;
            let pos = hash64 as usize & self.aligned_bucket_mask;
            let group = unsafe { Group::load(self.ctrl(pos)) };

            for bit in group.match_tag(tag_hash) {
                let index = pos + bit;
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

            let tag64 = scramble_tag(tag_hash);
            hash64 = hash64 ^ tag64;
        }
    }

    #[inline(always)]
    pub unsafe fn erase_index(&mut self, index: usize) {
        self.set_ctrl(index, Tag::EMPTY);
        self.items_until_growth += 1;
    }

    #[inline(always)]
    unsafe fn ctrl(&self, index: usize) -> *mut Tag {
        Self::ctrl_static(self.ctrl, index)
    }

    #[inline(always)]
    unsafe fn ctrl_static(ctrl: NonNull<u8>, index: usize) -> *mut Tag {
        ctrl.as_ptr().add(index).cast()
    }

    #[inline(always)]
    unsafe fn bucket(&self, index: usize) -> *mut (u64, V) {
        Self::bucket_static(self.ctrl, index)
    }

    #[inline(always)]
    unsafe fn bucket_static(ctrl: NonNull<u8>, index: usize) -> *mut (u64, V) {
        let data_end: *mut (u64, V) = ctrl.as_ptr().cast();
        data_end.sub(index + 1)
    }

    #[inline(always)]
    unsafe fn set_ctrl(&self, index: usize, tag: Tag) {
        *self.ctrl(index) = tag;
    }
}

impl<V: Copy> Drop for HashTable<V> {
    fn drop(&mut self) {
        unsafe { Self::dealloc(self.ctrl, self.num_buckets()) };
    }
}

fn scramble_tag(tag: Tag) -> u64 {
    (tag.0 as u64).wrapping_mul(MUL).rotate_left(32)
}

#[track_caller]
fn check_key_in_bucket(key: u64, bucket: usize, aligned_bucket_mask: usize, seed: u64) {
    let hash0 = fold_hash_fast(key, seed);
    let tag_hash = Tag::full(hash0);
    let hash1 = hash0 ^ scramble_tag(tag_hash);
    let is_first_choice = hash0 as usize & aligned_bucket_mask == bucket;
    let is_second_choice = hash1 as usize & aligned_bucket_mask == bucket;
    assert!(
        is_first_choice || is_second_choice,
        "{hash0:x}, {hash1:x}, {bucket:x}, {mask:x}",
        mask = aligned_bucket_mask
    );
}

const MUL: u64 = 0x2d35_8dcc_aa6c_78a5;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_basic_insert_and_get() {
        let mut table = HashTable::with_capacity(16);

        // Test basic insertion
        let (inserted, _, _) = table.insert(42, 100);
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
        let (inserted, _, _) = table.insert(123, 456);
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Update with new value
        let (inserted, _, _) = table.insert(123, 789);
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
            let (inserted, _, _) = table.insert(i, i * 10);
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

            let (cuckoo_inserted, _, _) = cuckoo_table.insert(key, value);
            let std_existed = std_map.insert(key, value).is_some();

            // Check insertion result consistency
            assert_eq!(cuckoo_inserted, !std_existed);
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

            let (cuckoo_inserted, _, _) = cuckoo_table.insert(key, value);
            let std_existed = std_map.insert(key, value).is_some();

            assert_eq!(cuckoo_inserted, !std_existed);
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
            let (inserted, _, _) = table.insert(key, key);
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
                if !std_map.contains_key(&k) {
                    break k;
                }
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
                let (cuckoo_inserted, _, _) = cuckoo_table.insert(i, new_value);
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

                    let (cuckoo_inserted, _, _) = cuckoo_table.insert(key, value);
                    let std_existed = std_map.insert(key, value).is_some();
                    assert_eq!(cuckoo_inserted, !std_existed);
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
                _ => unreachable!(),
            }
        }

        // Final consistency check
        assert_eq!(cuckoo_table.len(), std_map.len());

        for (&key, &value) in &std_map {
            assert_eq!(cuckoo_table.get(&key), Some(&value));
        }
    }
}
