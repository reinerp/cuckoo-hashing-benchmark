//! "Direct SIMD" layout which does SIMD probing on `[u64; 4]` rather than `[u8; 8]`.

use std::mem::MaybeUninit;

use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::{TRACK_PROBE_LENGTH, control64};

pub struct HashTable<V> {
    table: Box<[Bucket<V>]>,
    bucket_count: usize,
    len: usize,
    zero_value: Option<V>,
    seed: u64,
    total_probe_length: usize,
    rng: fastrand::Rng,
}

const BUCKET_SIZE: usize = 4;

#[repr(align(64))] // Cache line alignment
struct Bucket<V> {
    keys: [u64; BUCKET_SIZE],
    values: [MaybeUninit<V>; BUCKET_SIZE],
}

impl<V> HashTable<V> {
    pub fn print_stats(&self) {}

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7)
            .next_power_of_two()
            .div_ceil(BUCKET_SIZE) + 1;
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
            bucket_count: num_buckets,
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
    pub fn insert(&mut self, mut key: u64, mut value: V) -> (bool, (usize, usize)) {
        if key == 0 {
            let inserted = self.zero_value.is_none();
            self.len += inserted as usize;
            self.zero_value = Some(value);
            return (inserted, (usize::MAX, usize::MAX));
        }
        let bucket_count = self.bucket_count;
        let hash64 = fold_hash_fast(key, self.seed);

        let (existing_bucket, existing_index) = 'existing: loop {
            // Probe first group for a match.
            let pos0 = mul_hi(hash64 as usize, bucket_count);
            let keys0 = unsafe { self.table.get_unchecked(pos0) }.keys;

            if let Some(index) = control64::search(key, keys0) {
                break 'existing (pos0, index);
            }

            // Probe second group for a match.
            let pos1 = other_pos(pos0, hash64, bucket_count);
            let keys1 = unsafe { self.table.get_unchecked(pos1) }.keys;
            if let Some(index) = control64::search(key, keys1) {
                break 'existing (pos1, index);
            }

            // No match. We're going to insert; do BFS cuckoo loop.
            //
            // BFS queue maintains bucket indexes to visit.
            //
            // We search two complete N-ary trees, where N=BUCKET_SIZE. We search up to depth D=3, i.e.
            // 2 groups at the first level, 2*N, 2*N^2, 2*N^3.
            //
            // The parent of node at index `i` is at index `(i-2)/N`. Inversely, the first child of
            // node `j` is at index `j*N+2`.
            self.len += 1;
            const N: usize = BUCKET_SIZE;
            const BFS_MAX_LEN: usize = 2 * (1 + N + N * N + N * N * N);

            let seed = self.seed;
            let mut pos0 = pos0;
            let mut pos1 = pos1;
            let mut keys0 = keys0;
            let mut keys1 = keys1;

            let mut bfs_queue = [MaybeUninit::<usize>::uninit(); BFS_MAX_LEN];
            bfs_queue[0].write(pos0);
            bfs_queue[1].write(pos1);
            let mut bfs_read_pos = 0;
            let (mut path_index, mut bucket_index, mut bucket_offset) = 'bfs: loop {
                if let Some(empty_pos) = control64::search(0, keys0) {
                    break 'bfs (bfs_read_pos + 0, pos0, empty_pos);
                }
                if let Some(empty_pos) = control64::search(0, keys1) {
                    break 'bfs (bfs_read_pos + 1, pos1, empty_pos);
                }

                let bfs_write_pos = bfs_read_pos * N + 2;
                if bfs_write_pos < BFS_MAX_LEN {
                    for i in 0..N {
                        let other_pos = |pos: usize, key: u64| {
                            other_pos(pos, fold_hash_fast(key, seed), bucket_count)
                        };
                        let other_pos0 = other_pos(pos0, keys0[i]);
                        let other_pos1 = other_pos(pos1, keys1[i]);
                        unsafe {
                            *bfs_queue
                                .get_unchecked_mut(bfs_write_pos + i)
                                .write(other_pos0);
                            *bfs_queue
                                .get_unchecked_mut(bfs_write_pos + i + N)
                                .write(other_pos1);
                        }
                    }
                }

                bfs_read_pos += 2;

                if bfs_read_pos + 2 > BFS_MAX_LEN {
                    panic!("Failed to insert into cuckoo table; need to rehash");
                }
                pos0 = unsafe { bfs_queue[bfs_read_pos + 0].assume_init() };
                pos1 = unsafe { bfs_queue[bfs_read_pos + 1].assume_init() };
                keys0 = unsafe { self.table.get_unchecked(pos0) }.keys;
                keys1 = unsafe { self.table.get_unchecked(pos1) }.keys;
            };
            while path_index >= 2 {
                let parent_path_index = (path_index - 2) / N;
                let parent_bucket_offset = (path_index - 2) % N;
                let parent_bucket_index =
                    unsafe { bfs_queue.get_unchecked(parent_path_index).assume_init() };

                // Move from parent to child.
                unsafe {
                    let parent_bucket = self.table.get_unchecked(parent_bucket_index);
                    let parent_key = parent_bucket.keys[parent_bucket_offset];
                    let parent_value = parent_bucket.values[parent_bucket_offset].assume_init_read();
                    let child_bucket = self.table.get_unchecked_mut(bucket_index);
                    child_bucket.keys[bucket_offset] = parent_key;
                    child_bucket.values[bucket_offset].write(parent_value);
                }
                bucket_index = parent_bucket_index;
                bucket_offset = parent_bucket_offset;
                path_index = parent_path_index;
            }
            unsafe {
                let bucket = self.table.get_unchecked_mut(bucket_index);
                bucket.keys[bucket_offset] = key;
                bucket.values[bucket_offset].write(value);
            }
            return (true, (bucket_index, bucket_offset));
        };
        unsafe {
            *self.table.get_unchecked_mut(existing_bucket).values.get_unchecked_mut(existing_index).assume_init_mut() = value;
        }
        (false, (existing_bucket, existing_index))
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        if key == 0 {
            return self.zero_value.as_ref();
        }
        let mut hash64 = fold_hash_fast(key, self.seed);
        let bucket_count = self.bucket_count;
        let mut pos = mul_hi(hash64 as usize, bucket_count);
        for i in 0..2 {
            let bucket = unsafe { self.table.get_unchecked(pos) };
            let keys = bucket.keys;
            if let Some(index) = control64::search(key, keys) {
                return Some(unsafe { bucket.values.get_unchecked(index).assume_init_ref() });
            }
            if control64::search(0, keys).is_some() {
                return None;
            }
            pos = other_pos(pos, hash64, bucket_count);
        }
        None
    }

    #[inline(always)]
    pub fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, (bucket_index, bucket_offset)) = self.insert(key, value);
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
        }
    }
}

#[inline(always)]
fn mul_hi(a: usize, b: usize) -> usize {
    let a = a as u128;
    let b = b as u128;
    ((a * b) >> 64) as usize
}

// #[inline(always)]
// fn one_step_mod(a: usize, modulo: usize) -> usize {
//     let (sub, overflow) = a.overflowing_sub(modulo);
//     std::hint::select_unpredictable(overflow, a, sub)
// }

fn other_pos(pos: usize, hash: u64, bucket_count: usize) -> usize {
    // We compute:
    //   (pos - (hash * bucket_count)) % bucket_count
    //
    // This has the advantage of being idempotent.
    let pos2 = mul_hi(hash.rotate_left(32) as usize, bucket_count);
    let (sub, overflow) = pos2.overflowing_sub(pos);
    std::hint::select_unpredictable(overflow, sub.wrapping_add(bucket_count), sub)
}