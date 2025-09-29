//! "Direct SIMD + quadratic probing" layout which combines SIMD probing on `[u64; 4]` buckets
//! with quadratic probing for collision resolution instead of cuckoo hashing.

use std::mem::MaybeUninit;

use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::{TRACK_PROBE_LENGTH, control64};

pub struct HashTable<V> {
    table: Box<[Bucket<V>]>,
    bucket_mask: usize,
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
}

const BUCKET_SIZE: usize = 4;

#[repr(align(64))] // Cache line alignment
struct Bucket<V> {
    keys: [u64; BUCKET_SIZE],
    values: [MaybeUninit<V>; BUCKET_SIZE],
}

/// Probe sequence based on triangular numbers, which is guaranteed (since our
/// table size is a power of two) to visit every group of elements exactly once.
///
/// A triangular probe has us jump by 1 more group every time. So first we
/// jump by 1 group (meaning we just continue our linear scan), then 2 groups
/// (skipping over 1 group), then 3 groups (skipping over 2 groups), and so on.
#[derive(Clone)]
struct ProbeSeq {
    pos: usize,
    stride: usize,
}

impl ProbeSeq {
    #[inline]
    fn move_next(&mut self, bucket_mask: usize) {
        // We should have found an empty bucket by now and ended the probe.
        debug_assert!(
            self.stride <= bucket_mask,
            "Went past end of probe sequence"
        );

        self.stride += 1; // Increment by 1 bucket per step (triangular sequence)
        self.pos += self.stride;
        self.pos &= bucket_mask;
    }
}

impl<V> HashTable<V> {
    pub fn print_stats(&self) {
        if TRACK_PROBE_LENGTH && self.len > 0 {
            println!("  avg_probe_length: {}", self.total_probe_length as f64 / self.len as f64);
        }
    }

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7)
            .next_power_of_two()
            .div_ceil(BUCKET_SIZE);
        let table = {
            let mut v = Vec::new();
            v.resize_with(num_buckets, || Bucket {
                keys: [0; BUCKET_SIZE],
                values: std::array::from_fn(|_| MaybeUninit::uninit()),
            });
            v.into_boxed_slice()
        };
        let seed = fastrand::Rng::with_seed(123).u64(..);
        Self {
            table,
            bucket_mask: num_buckets - 1,
            len: 0,
            zero_value: None,
            seed,
            total_probe_length: 0,
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    fn probe_seq(&self, hash64: u64) -> ProbeSeq {
        ProbeSeq {
            pos: (hash64 as usize) & self.bucket_mask,
            stride: 0,
        }
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, (usize, usize), usize) {
        let mut insertion_probe_length = 1;
        if key == 0 {
            let inserted = self.zero_value.is_none();
            self.len += inserted as usize;
            self.zero_value = Some(value);
            return (inserted, (usize::MAX, usize::MAX), insertion_probe_length);
        }

        let hash64 = fold_hash_fast(key, self.seed);
        let mut probe_seq = self.probe_seq(hash64);
        let mut probe_count = 0;

        loop {
            let bucket = unsafe { self.table.get_unchecked(probe_seq.pos) };
            let keys = bucket.keys;

            // Check if key already exists in this bucket using SIMD
            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                // Key found, update value
                let index = mask.trailing_zeros() as usize / stride;
                unsafe {
                    *self.table.get_unchecked_mut(probe_seq.pos)
                        .values.get_unchecked_mut(index)
                        .assume_init_mut() = value;
                }
                return (false, (probe_seq.pos, index), insertion_probe_length);
            }

            // Look for empty slot (key == 0) in this bucket using SIMD
            let (empty_mask, stride) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                // Found empty slot, insert here
                let index = empty_mask.trailing_zeros() as usize / stride;
                unsafe {
                    let bucket = self.table.get_unchecked_mut(probe_seq.pos);
                    bucket.keys[index] = key;
                    bucket.values[index].write(value);
                }
                self.len += 1;
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += probe_count + 1;
                }
                insertion_probe_length = probe_count + 1;
                return (true, (probe_seq.pos, index), insertion_probe_length);
            }

            // No match and no empty slot, move to next bucket via quadratic probing
            probe_seq.move_next(self.bucket_mask);
            probe_count += 1;

            if probe_count > self.bucket_mask {
                panic!("Failed to insert into hash table; table is full");
            }
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        if key == 0 {
            return self.zero_value.as_ref();
        }

        let hash64 = fold_hash_fast(key, self.seed);
        let mut probe_seq = self.probe_seq(hash64);

        loop {
            let bucket = unsafe { self.table.get_unchecked(probe_seq.pos) };
            let keys = bucket.keys;

            // Check if key exists in this bucket using SIMD
            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                let index = mask.trailing_zeros() as usize / stride;
                return Some(unsafe { bucket.values.get_unchecked(index).assume_init_ref() });
            }

            // Check if there are any empty slots - if so, key definitely doesn't exist
            let (empty_mask, _) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                return None;
            }

            // Continue probing
            probe_seq.move_next(self.bucket_mask);
        }
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        if key == 0 {
            return (1, self.zero_value.is_some()); // Zero key is always in first probe
        }

        let hash64 = fold_hash_fast(key, self.seed);
        let mut probe_seq = self.probe_seq(hash64);
        let mut probe_count = 0;

        loop {
            probe_count += 1;
            let bucket = unsafe { self.table.get_unchecked(probe_seq.pos) };
            let keys = bucket.keys;

            // Check if key exists in this bucket using SIMD
            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                return (probe_count, true); // Key found
            }

            // Check if there are any empty slots in this bucket
            let (empty_mask, _) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                return (probe_count, false); // Empty slot found, key absent
            }

            // Continue probing
            probe_seq.move_next(self.bucket_mask);
        }
    }

    #[inline(always)]
    pub fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, (bucket_index, bucket_offset), _) = self.insert(key, value);
        if inserted {
            if key == 0 {
                self.zero_value = None;
            } else {
                unsafe {
                    let bucket = self.table.get_unchecked_mut(bucket_index);
                    *bucket.keys.get_unchecked_mut(bucket_offset) = 0;
                    bucket.values.get_unchecked_mut(bucket_offset).assume_init_drop();
                }
            }
            self.len -= 1; // Decrement length after erase
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_insert_and_get() {
        let mut table = HashTable::with_capacity(16);

        // Test basic insertion
        let (inserted, _) = table.insert(42, "hello");
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Test retrieval
        assert_eq!(table.get(&42), Some(&"hello"));
        assert_eq!(table.get(&999), None);
    }

    #[test]
    fn test_zero_key() {
        let mut table = HashTable::with_capacity(16);

        // Test zero key insertion
        let (inserted, _) = table.insert(0, "zero");
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Test zero key retrieval
        assert_eq!(table.get(&0), Some(&"zero"));
    }

    #[test]
    fn test_update_existing() {
        let mut table = HashTable::with_capacity(16);

        // Insert initial value
        let (inserted, _) = table.insert(123, "first");
        assert!(inserted);
        assert_eq!(table.len(), 1);

        // Update with new value
        let (inserted, _) = table.insert(123, "updated");
        assert!(!inserted); // Should be false since key already existed
        assert_eq!(table.len(), 1); // Length should remain the same

        // Verify updated value
        assert_eq!(table.get(&123), Some(&"updated"));
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
    fn test_collision_handling() {
        let mut table = HashTable::with_capacity(8); // Small table to force collisions

        // Insert many values to test quadratic probing
        let keys = [1, 17, 33, 49, 65, 81, 97]; // These may collide depending on hash function

        for &key in &keys {
            let (inserted, _) = table.insert(key, key * 100);
            assert!(inserted);
        }

        // Verify all keys can be retrieved
        for &key in &keys {
            assert_eq!(table.get(&key), Some(&(key * 100)));
        }
    }

    #[test]
    fn test_insert_and_erase() {
        let mut table = HashTable::with_capacity(16);

        // Insert and immediately erase
        table.insert_and_erase(42, "test");

        // Should not be found since it was erased
        assert_eq!(table.get(&42), None);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn test_bucket_simd_search() {
        let mut table = HashTable::with_capacity(64);

        // Insert values to ensure we test the SIMD search within buckets
        // Use a smaller number to avoid filling the table
        for i in 1..=20 {
            table.insert(i, i * 2);
        }

        // Verify all values are retrievable
        for i in 1..=20 {
            assert_eq!(table.get(&i), Some(&(i * 2)));
        }
    }
}