//! A dense_hash_set for u64 keys.
//!
//! Compared to std::collections::HashSet<u64>, this uses a different layout: no metadata table, just plain data.
//! This is similar to Google's dense_hash_map, which predates the SwissTable design. By avoiding a metadata table,
//! we may need to do longer probe sequences (each probe is 8 bytes, not 1 byte), but on the other hand we only take
//! 1 cache miss per access, not 2.

use std::mem::MaybeUninit;

use crate::TRACK_PROBE_LENGTH;
use crate::u64_fold_hash_fast::fold_hash_fast;

pub struct U64HashSet<V: Copy> {
    table: Box<[(u64, MaybeUninit<V>)]>,
    bucket_mask: usize,
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
    rng: fastrand::Rng,
}

const WINDOW_SIZE: usize = 2;

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
        let num_buckets = ((capacity * 8) / 7).next_power_of_two();
        let table = vec![(0u64, MaybeUninit::uninit()); num_buckets].into_boxed_slice();
        let seed = fastrand::Rng::with_seed(123).u64(..);
        Self {
            table,
            bucket_mask: num_buckets - 1,
            len: 0,
            zero_value: None,
            seed,
            total_probe_length: 0,
            rng: fastrand::Rng::with_seed(123),
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn insert(&mut self, mut key: u64, mut value: V) -> (bool, usize) {
        if key == 0 {
            let inserted = self.zero_value.is_none();
            self.len += inserted as usize;
            self.zero_value = Some(value);
            return (inserted, usize::MAX);
        }
        let bucket_mask = self.bucket_mask;

        loop {
            let mut hash64 = fold_hash_fast(key, self.seed);
            let mut bucket_i = hash64;
            let mut probe_length = 1;
            for i in 0..2 {
                for j in 0..WINDOW_SIZE {
                    let bucket_pos = (bucket_i as usize + j) & bucket_mask;
                    let element = unsafe { self.table.get_unchecked_mut(bucket_pos) };
                    if element.0 == 0 {
                        element.0 = key;
                        element.1.write(value);
                        self.len += 1;
                        if TRACK_PROBE_LENGTH {
                            self.total_probe_length += probe_length;
                        }
                        return (true, bucket_pos);
                    }
                    if element.0 == key {
                        element.1.write(value);
                        return (false, bucket_pos);
                    }
                    probe_length += 1;
                }
                bucket_i = bucket_i.rotate_left(32);
            }

            let rng_next = self.rng.usize(..);
            let evict_pos = (hash64.rotate_left(32 * (rng_next % 2) as u32) as usize
                + ((rng_next / 2) % WINDOW_SIZE))
                & bucket_mask;
            let (new_key, new_value) = std::mem::replace(
                unsafe { self.table.get_unchecked_mut(evict_pos) },
                (key, MaybeUninit::new(value)),
            );
            key = new_key;
            value = unsafe { new_value.assume_init() };
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        if key == 0 {
            return self.zero_value.as_ref();
        }
        let mut hash64 = fold_hash_fast(key, self.seed);
        let bucket_mask = self.bucket_mask;
        // let mut result = None;
        for i in 0..2 {
            let mut result = None;
            // Safety: bucket_mask is correct because the number of buckets is a power of 2.
            for j in 0..WINDOW_SIZE {
                let bucket_pos = (hash64 as usize + j) & bucket_mask;
                let element = unsafe { self.table.get_unchecked(bucket_pos) };
                result = std::hint::select_unpredictable(element.0 == key, Some(unsafe { &self.table.get_unchecked(bucket_pos).1 }), result);
            }
            if let Some(result) = result {
                return Some(unsafe { result.assume_init_ref() });
            }
            hash64 = hash64.rotate_left(32);
        }
        None
        // result.map(|result| unsafe { result.assume_init_ref() })
    }

    #[inline(always)]
    pub fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, index) = self.insert(key, value);
        if inserted {
            if key == 0 {
                self.zero_value = None;
            } else {
                unsafe {
                    self.table.get_unchecked_mut(index).0 = 0;
                }
            }
        }
    }
}
