//! "Direct SIMD + linear probing": SIMD probing on aligned `[u64; 4]` cache-line buckets, with
//! *linear* probing for collision resolution. Identical to `direct_simd_quadratic_probing` except
//! the probe sequence steps by exactly 1 bucket each probe (`pos = (pos + 1) & mask`), with no
//! running stride.

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

/// Linear probe sequence: step by one bucket each probe.
#[derive(Clone)]
struct ProbeSeq {
    pos: usize,
}

impl ProbeSeq {
    #[inline]
    fn move_next(&mut self, bucket_mask: usize) {
        self.pos = (self.pos + 1) & bucket_mask;
    }
}

impl<V> HashTable<V> {
    pub fn print_stats(&self) {
        if TRACK_PROBE_LENGTH && self.len > 0 {
            println!(
                "  avg_probe_length: {}",
                self.total_probe_length as f64 / self.len as f64
            );
        }
    }

    #[inline(always)]
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
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

            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                let index = mask.trailing_zeros() as usize / stride;
                unsafe {
                    *self
                        .table
                        .get_unchecked_mut(probe_seq.pos)
                        .values
                        .get_unchecked_mut(index)
                        .assume_init_mut() = value;
                }
                return (false, (probe_seq.pos, index), insertion_probe_length);
            }

            let (empty_mask, stride) = control64::search_mask(0, keys);
            if empty_mask != 0 {
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

            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                let index = mask.trailing_zeros() as usize / stride;
                return Some(unsafe { bucket.values.get_unchecked(index).assume_init_ref() });
            }

            let (empty_mask, _) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                return None;
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        if key == 0 {
            return (1, self.zero_value.is_some());
        }

        let hash64 = fold_hash_fast(key, self.seed);
        let mut probe_seq = self.probe_seq(hash64);
        let mut probe_count = 0;

        loop {
            probe_count += 1;
            let bucket = unsafe { self.table.get_unchecked(probe_seq.pos) };
            let keys = bucket.keys;

            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                return (probe_count, true);
            }

            let (empty_mask, _) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                return (probe_count, false);
            }

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
                    bucket
                        .values
                        .get_unchecked_mut(bucket_offset)
                        .assume_init_drop();
                }
            }
            self.len -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn cross_check_std_randomized() {
        for seed in [1u64, 2, 7, 42, 100] {
            let mut rng = fastrand::Rng::with_seed(seed);
            // Fixed-capacity table (no auto-grow); size for the insert count.
            let mut table = HashTable::<u64>::with_capacity(3000);
            let mut std_map: HashMap<u64, u64> = HashMap::new();
            for _ in 0..3000 {
                let key = rng.u64(..);
                let val = rng.u64(..);
                table.insert(key, val);
                std_map.insert(key, val);
            }
            assert_eq!(table.len(), std_map.len(), "len mismatch seed={seed}");
            for (&k, &v) in &std_map {
                assert_eq!(table.get(&k).copied(), Some(v), "missing key {k:x} seed={seed}");
            }
            for _ in 0..3000 {
                let k = rng.u64(..);
                assert_eq!(table.get(&k).copied(), std_map.get(&k).copied(), "absent mismatch {k:x}");
            }
        }
    }
}
