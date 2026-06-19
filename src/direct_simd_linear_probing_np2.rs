//! "Direct SIMD + linear probing, NON-power-of-2 sizing": same as `direct_simd_linear_probing`
//! but the bucket count may be ANY integer, and indexing uses the high-multiply map
//! `mul_high(hash, num_buckets)` instead of `hash & mask`. The linear step wraps with a compare
//! (`pos += 1; if pos == num_buckets { pos = 0 }`) instead of a mask.
//!
//! Purpose: isolate the *price* of the user's reason (a) "support for non-power-of-2 tables".
//! Quadratic probing CANNOT be sized non-power-of-2 (its triangular sequence only covers a pow2
//! table), so this freedom is unique to linear/cuckoo. The freedom's *benefit* is memory (you can
//! sit at any chosen load factor instead of rounding the table size up to the next power of two,
//! which wastes up to ~2x memory); the *cost* is that `mul_high` is a latency-3 multiply vs the
//! latency-1 `& mask`. This table measures that cost head-to-head against `direct_simd_linear_probing`
//! by sizing to the *same* bucket count but using the general indexing path.

use std::mem::MaybeUninit;

use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::{TRACK_PROBE_LENGTH, control64};

pub struct HashTable<V> {
    table: Box<[Bucket<V>]>,
    num_buckets: usize, // arbitrary; NOT necessarily a power of two
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
}

const BUCKET_SIZE: usize = 4;

#[repr(align(64))]
struct Bucket<V> {
    keys: [u64; BUCKET_SIZE],
    values: [MaybeUninit<V>; BUCKET_SIZE],
}

#[inline(always)]
fn mul_high(x: u64, y: u64) -> u64 {
    (((x as u128) * (y as u128)) >> 64) as u64
}

impl<V> HashTable<V> {
    pub fn print_stats(&self) {}

    #[inline(always)]
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // Size to the SAME bucket count as the pow2 linear table, so the only measured difference
        // is the indexing arithmetic (mul_high + compare-wrap vs mask). (A real deployment would
        // instead pick an arbitrary num_buckets to hit an exact target load factor.)
        let num_buckets = ((capacity * 8) / 7)
            .next_power_of_two()
            .div_ceil(BUCKET_SIZE);
        Self::with_num_buckets(num_buckets)
    }

    /// Construct with an explicit (possibly non-power-of-2) bucket count.
    pub fn with_num_buckets(num_buckets: usize) -> Self {
        assert!(num_buckets >= 1);
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
            num_buckets,
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

    #[cfg(test)]
    pub fn num_buckets(&self) -> usize {
        self.num_buckets
    }

    #[cfg(test)]
    pub fn occupancy_stats(&self) -> (usize, usize, f64) {
        let mut maxc = 0;
        let mut total = 0;
        for b in self.table.iter() {
            let c = b.keys.iter().filter(|&&k| k != 0).count();
            maxc = maxc.max(c);
            total += c;
        }
        (self.table.len(), maxc, total as f64 / self.table.len() as f64)
    }

    #[inline(always)]
    fn home(&self, hash64: u64) -> usize {
        mul_high(hash64, self.num_buckets as u64) as usize
    }

    #[inline(always)]
    fn step(&self, pos: usize) -> usize {
        let next = pos + 1;
        if next == self.num_buckets { 0 } else { next }
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
        let mut pos = self.home(hash64);
        let mut probe_count = 0;

        loop {
            let bucket = unsafe { self.table.get_unchecked(pos) };
            let keys = bucket.keys;

            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                let index = mask.trailing_zeros() as usize / stride;
                unsafe {
                    *self
                        .table
                        .get_unchecked_mut(pos)
                        .values
                        .get_unchecked_mut(index)
                        .assume_init_mut() = value;
                }
                return (false, (pos, index), insertion_probe_length);
            }

            let (empty_mask, stride) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                let index = empty_mask.trailing_zeros() as usize / stride;
                unsafe {
                    let bucket = self.table.get_unchecked_mut(pos);
                    bucket.keys[index] = key;
                    bucket.values[index].write(value);
                }
                self.len += 1;
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += probe_count + 1;
                }
                insertion_probe_length = probe_count + 1;
                return (true, (pos, index), insertion_probe_length);
            }

            pos = self.step(pos);
            probe_count += 1;
            if probe_count > self.num_buckets {
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
        let mut pos = self.home(hash64);

        loop {
            let bucket = unsafe { self.table.get_unchecked(pos) };
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

            pos = self.step(pos);
        }
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        if key == 0 {
            return (1, self.zero_value.is_some());
        }
        let hash64 = fold_hash_fast(key, self.seed);
        let mut pos = self.home(hash64);
        let mut probe_count = 0;
        loop {
            probe_count += 1;
            let bucket = unsafe { self.table.get_unchecked(pos) };
            let keys = bucket.keys;
            let (mask, stride) = control64::search_mask(key, keys);
            if mask != 0 {
                return (probe_count, true);
            }
            let (empty_mask, _) = control64::search_mask(0, keys);
            if empty_mask != 0 {
                return (probe_count, false);
            }
            pos = self.step(pos);
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
    fn find_hit_path_all_present() {
        // Mirrors the benchmark's find_hit setup via with_capacity (power-of-2 bucket count),
        // at the highest load (87.5%). Asserts every inserted key is found (catches an
        // insert/get home-bucket inconsistency that would make get() return None quickly).
        let capacity = 896; // => 256 buckets => 1024 slots
        let n = 895usize; // ~87.4% load
        let mut table = HashTable::<u64>::with_capacity(capacity);
        let mut rng = fastrand::Rng::with_seed(123);
        let mut keys: Vec<u64> = (0..n as u64).collect();
        rng.shuffle(&mut keys);
        for &k in &keys {
            table.insert(k, k);
        }
        assert_eq!(table.len(), n, "not all keys inserted");
        let (nb, maxc, avgc) = table.occupancy_stats();
        eprintln!("np2: num_buckets={nb} (slots={}), max_per_bucket={maxc}, avg_per_bucket={avgc:.3}", nb * 4);
        let mut total_probe = 0usize;
        for k in 0..n as u64 {
            let (pl, found) = table.probe_length(k);
            assert!(found, "key {k} not found at load 87.5%");
            total_probe += pl;
        }
        let avg = total_probe as f64 / n as f64;
        // NOTE: with SEQUENTIAL keys (0..n) and mul_high (Fibonacci) indexing, the high hash bits
        // are near-perfectly equidistributed, so buckets fill to exactly capacity with ~no overflow
        // and avg hit probe length is ~1.0 even at 87.5% load. This is a real (Fibonacci-hashing)
        // effect, NOT a correctness bug — every key is still found. It does mean the find_hit
        // benchmark's sequential-key workload is not representative for np2; price the mul_high tax
        // from find_miss (random keys) instead.
        eprintln!("np2 find_hit avg probe length at 87.5% load (sequential keys) = {avg:.3}");
        assert!(avg >= 1.0);
    }

    #[test]
    fn cross_check_std_nonpow2_sizes() {
        // Deliberately non-power-of-2 bucket counts.
        for (nb, seed) in [(1009usize, 1u64), (1500, 2), (3001, 7), (5000, 42)] {
            let mut rng = fastrand::Rng::with_seed(seed);
            let mut table = HashTable::<u64>::with_num_buckets(nb);
            let mut std_map: HashMap<u64, u64> = HashMap::new();
            let n = nb * 4 * 3 / 4; // ~75% load
            for _ in 0..n {
                let key = rng.u64(..);
                let val = rng.u64(..);
                table.insert(key, val);
                std_map.insert(key, val);
            }
            assert_eq!(table.len(), std_map.len(), "len mismatch nb={nb}");
            for (&k, &v) in &std_map {
                assert_eq!(table.get(&k).copied(), Some(v), "missing key {k:x} nb={nb}");
            }
            for _ in 0..n {
                let k = rng.u64(..);
                assert_eq!(table.get(&k).copied(), std_map.get(&k).copied());
            }
        }
    }
}
