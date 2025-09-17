//! A dense_hash_set for u64 keys.
//!
//! Compared to std::collections::HashSet<u64>, this uses a different layout: no metadata table, just plain data.
//! This is similar to Google's dense_hash_map, which predates the SwissTable design. By avoiding a metadata table,
//! we may need to do longer probe sequences (each probe is 8 bytes, not 1 byte), but on the other hand we only take
//! 1 cache miss per access, not 2.

use std::mem::MaybeUninit;

use crate::u64_fold_hash_fast::fold_hash_fast;

pub struct U64HashSet<V: Copy> {
    table: Box<[(u64, MaybeUninit<V>)]>,
    bucket_mask: usize,
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
}

impl<V: Copy> U64HashSet<V> {
    pub fn print_stats(&self) {
        println!(
            "  avg_probe_length: {}",
            self.total_probe_length as f64 / self.len as f64
        );
    }

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7).next_power_of_two() * 2;
        let table = vec![(0u64, MaybeUninit::uninit()); num_buckets].into_boxed_slice();
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

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, usize) {
        if key == 0 {
            let inserted = self.zero_value.is_none();
            self.len += inserted as usize;
            self.zero_value = Some(value);
            return (inserted, usize::MAX);
        }
        let hash64 = fold_hash_fast(key, self.seed);
        let bucket_mask = self.bucket_mask;
        let mut bucket_i = hash64 as usize;

        let mut probe_length = 1;
        loop {
            // Safety: bucket_mask is correct because the number of buckets is a power of 2.
            let element = unsafe { self.table.get_unchecked_mut(bucket_i & bucket_mask) };
            if element.0 == 0 {
                element.0 = key;
                self.len += 1;
                self.total_probe_length += probe_length;
                return (true, bucket_i);
            }
            if element.0 == key {
                return (false, bucket_i);
            }
            probe_length += 1;
            bucket_i += 1;
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let hash64 = fold_hash_fast(key, self.seed);
        let bucket_mask = self.bucket_mask;
        let mut bucket_i = hash64 as usize;
        loop {
            // Safety: bucket_mask is correct because the number of buckets is a power of 2.
            let bucket_pos = bucket_i & bucket_mask;
            let element = unsafe { self.table.get_unchecked_mut(bucket_pos) };
            if element.0 == key {
                return Some(unsafe { self.table.get_unchecked(bucket_pos).1.assume_init_ref() });
            } else if element.0 == 0 {
                return None;
            }
            bucket_i += 1;
        }
    }
}
