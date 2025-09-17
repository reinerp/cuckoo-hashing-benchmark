//! A dense_hash_set for u64 keys.
//! 
//! Compared to std::collections::HashSet<u64>, this uses a different layout: no metadata table, just plain data.
//! This is similar to Google's dense_hash_map, which predates the SwissTable design. By avoiding a metadata table,
//! we may need to do longer probe sequences (each probe is 8 bytes, not 1 byte), but on the other hand we only take
//! 1 cache miss per access, not 2.

use std::mem::MaybeUninit;

use crate::u64_fold_hash_fast::fold_hash_fast;

pub struct U64HashSet<V: Copy> {
    table: Box<[Bucket<V>]>,
    bucket_mask: usize,
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
}

const BUCKET_SIZE: usize = 8;

#[derive(Clone, Copy)]
#[repr(align(64))] // Cache line alignment
struct Bucket<V: Copy>([(u64, MaybeUninit<V>); BUCKET_SIZE]);

impl<V: Copy> U64HashSet<V> {
    pub fn print_stats(&self) {
        println!("  avg_probe_length: {}", self.total_probe_length as f64 / self.len as f64);
    }

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7).next_power_of_two().div_ceil(BUCKET_SIZE) * 2;
        let table = vec![Bucket([(0u64, MaybeUninit::uninit()); BUCKET_SIZE]); num_buckets].into_boxed_slice();
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
        let element_offset_in_bucket = (hash64 >> 61) as usize;
        let mut bucket_i = hash64 as usize;


        let mut probe_length = 1;
        loop {
            // Safety: bucket_mask is correct because the number of buckets is a power of 2.
            let bucket = unsafe { self.table.get_unchecked_mut(bucket_i & bucket_mask) };
            for element_i in 0..BUCKET_SIZE {
                let element = &mut bucket.0[(element_i + element_offset_in_bucket) % BUCKET_SIZE];
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
            }
            bucket_i += 1;
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let hash64 = fold_hash_fast(key, self.seed);
        let bucket_mask = self.bucket_mask;
        let element_offset_in_bucket = (hash64 >> 61) as usize;
        let mut bucket_i = hash64 as usize;
        loop {
            // Safety: bucket_mask is correct because the number of buckets is a power of 2.
            let bucket_pos = bucket_i & bucket_mask;
            let bucket = unsafe { self.table.get_unchecked_mut(bucket_pos) };
            for element_i in 0..BUCKET_SIZE {
                let element_pos = (element_i + element_offset_in_bucket) % BUCKET_SIZE;
                let element = &mut bucket.0[element_pos];
                if element.0 == key {
                    return Some(unsafe { self.table.get_unchecked(bucket_pos).0[element_pos].1.assume_init_ref() });
                } else if element.0 == 0 {
                    return None;
                }
            }
            bucket_i += 1;
        }
    }
}