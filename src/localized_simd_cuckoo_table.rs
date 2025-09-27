//! "Direct SIMD" layout which does SIMD probing on `[u64; 4]` rather than `[u8; 8]`.

use std::hint::likely;
use std::mem::MaybeUninit;

use crate::control::{Group, Tag};
use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::{TRACK_PROBE_LENGTH, control64};

pub struct HashTable<V> {
    table: Box<[Bucket<V>]>,
    bucket_mask: usize,
    len: usize,
    seed: u64,
    total_probe_length: usize,
    rng: fastrand::Rng,
}

const BUCKET_SIZE: usize = 7;

#[repr(C)]
#[repr(align(128))] // Cache line alignment
struct Bucket<V> {
    keys: [u64; BUCKET_SIZE],
    // TODO: 1 byte "overflow" flag?
    fprints: [Tag; BUCKET_SIZE + 1],
    values: [MaybeUninit<V>; BUCKET_SIZE],
}

impl<V> HashTable<V> {
    pub fn print_stats(&self) {}

    #[inline(always)]
    pub fn with_capacity(capacity: usize) -> Self {
        // TODO: integer overflow...
        let num_buckets = ((capacity * 8) / 7).div_ceil(BUCKET_SIZE + 1)
            .next_power_of_two();
        let table = {
            let mut v = Vec::new();
            v.resize_with(num_buckets, || Bucket {
                fprints: {
                    let mut fprints = [Tag::EMPTY; BUCKET_SIZE + 1];
                    fprints[BUCKET_SIZE] = Tag::DELETED;
                    fprints
                },
                keys: [0; BUCKET_SIZE],
                values: std::array::from_fn(|_| MaybeUninit::uninit()),
            });
            v.into_boxed_slice()
        };
        let seed = fastrand::Rng::with_seed(123).u64(..);
        Self {
            table,
            bucket_mask: (num_buckets - 1) * std::mem::size_of::<Bucket<V>>(),
            len: 0,
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
        let bucket_mask = self.bucket_mask;
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);

        let (existing_bucket, existing_index) = 'existing: loop {
            // Probe first group for a match.
            let pos0 = hash64 as usize & bucket_mask;
            let bucket0 = unsafe { self.bucket(pos0) };
            assert!(Group::WIDTH == BUCKET_SIZE + 1);
            let group0 = unsafe { Group::load(bucket0.fprints.as_ptr().cast()) };

            for bit in group0.match_tag(tag_hash) {
                if likely(unsafe { *bucket0.keys.get_unchecked(bit) } == key) {
                    break 'existing (pos0, bit);
                }
            }

            // Probe second group for a match.
            let pos1 = (hash64 ^ scramble_tag(tag_hash)) as usize & self.bucket_mask;
            let bucket1 = unsafe { self.bucket(pos1) };
            let group1 = unsafe { Group::load(bucket1.fprints.as_ptr().cast()) };
            for bit in group1.match_tag(tag_hash) {
                if likely(unsafe { *bucket1.keys.get_unchecked(bit) } == key) {
                    break 'existing (pos1, bit);
                }
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
            let mut group0 = group0;
            let mut group1 = group1;

            let mut bfs_queue = [MaybeUninit::<usize>::uninit(); BFS_MAX_LEN];
            bfs_queue[0].write(pos0);
            bfs_queue[1].write(pos1);
            let mut bfs_read_pos = 0;
            let (mut path_index, mut bucket_index, mut bucket_offset) = 'bfs: loop {
                if let Some(empty_pos) = group0.match_empty().lowest_set_bit() {
                    break 'bfs (bfs_read_pos + 0, pos0, empty_pos);
                }
                if let Some(empty_pos) = group1.match_empty().lowest_set_bit() {
                    break 'bfs (bfs_read_pos + 1, pos1, empty_pos);
                }

                let bfs_write_pos = bfs_read_pos * N + 2;
                if bfs_write_pos < BFS_MAX_LEN {
                    for i in 0..N {
                        let other_pos = |pos: usize| {
                            let tag = unsafe { *self.bucket(pos).fprints.get_unchecked(i) };
                            pos ^ (scramble_tag(tag) as usize & bucket_mask)
                        };
                        let other_pos0 = other_pos(pos0);
                        let other_pos1 = other_pos(pos1);
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
                group0 = unsafe { Group::load(self.bucket(pos0).fprints.as_ptr().cast()) };
                group1 = unsafe { Group::load(self.bucket(pos1).fprints.as_ptr().cast()) };
            };
            while path_index >= 2 {
                let parent_path_index = (path_index - 2) / N;
                let parent_bucket_offset = (path_index - 2) % N;
                let parent_bucket_index =
                    unsafe { bfs_queue.get_unchecked(parent_path_index).assume_init() };

                // Move from parent to child.
                unsafe {
                    let parent_bucket = self.bucket_mut(parent_bucket_index);
                    let parent_tag = parent_bucket.fprints[parent_bucket_offset];
                    let parent_key = parent_bucket.keys[parent_bucket_offset];
                    let parent_value = parent_bucket.values[parent_bucket_offset].assume_init_read();

                    let child_bucket = self.bucket_mut(bucket_index);
                    child_bucket.fprints[bucket_offset] = parent_tag;
                    child_bucket.keys[bucket_offset] = parent_key;
                    child_bucket.values[bucket_offset].write(parent_value);
                }
                bucket_index = parent_bucket_index;
                bucket_offset = parent_bucket_offset;
                path_index = parent_path_index;
            }
            unsafe {
                let bucket = self.bucket_mut(bucket_index);
                bucket.fprints[bucket_offset] = tag_hash;
                bucket.keys[bucket_offset] = key;
                bucket.values[bucket_offset].write(value);
            }
            return (true, (bucket_index, bucket_offset));
        };
        unsafe {
            *self.bucket_mut(existing_bucket).values.get_unchecked_mut(existing_index).assume_init_mut() = value;
        }
        (false, (existing_bucket, existing_index))
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let mut hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let bucket_mask = self.bucket_mask;
        for i in 0..2 {
            let bucket = unsafe { self.bucket(hash64 as usize & bucket_mask) };
            assert!(Group::WIDTH == BUCKET_SIZE + 1);
            let group = unsafe { Group::load(bucket.fprints.as_ptr().cast()) };

            let matches = group.match_tag(tag_hash);
            if matches.any_bit_set() {
                for bit in group.match_tag(tag_hash) {
                    if likely(unsafe { *bucket.keys.get_unchecked(bit) } == key) {
                        return Some(unsafe { bucket.values.get_unchecked(bit).assume_init_ref() });
                    }
                }
            }

            // if i == 1 || group.match_empty().any_bit_set() {
            //     return None;
            // }

            // // Only return None if this is the second location AND there are empty slots
            // if i == 1 {
            //     return None;
            // }

            hash64 ^= scramble_tag(tag_hash);
        }
        None
    }

    #[inline(always)]
    pub fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, (bucket_index, bucket_offset)) = self.insert(key, value);
        if inserted {
            unsafe {
                let bucket = self.bucket_mut(bucket_index);
                bucket.fprints[bucket_offset] = Tag::EMPTY;
                *bucket.keys.get_unchecked_mut(bucket_offset) = 0;
                bucket.values.get_unchecked_mut(bucket_offset).assume_init_drop();
            }
            self.len -= 1;
        }
    }

    #[inline(always)]
    unsafe fn bucket(&self, masked_position: usize) -> &Bucket<V> {
        unsafe {
            &*self.table.as_ptr().byte_add(masked_position)
        }
    }

    #[inline(always)]
    unsafe fn bucket_mut(&mut self, masked_position: usize) -> &mut Bucket<V> {
        unsafe {
            &mut *self.table.as_mut_ptr().byte_add(masked_position)
        }
    }
}

fn scramble_tag(tag: Tag) -> u64 {
    (tag.0 as u64).wrapping_mul(MUL).rotate_left(32)
}

const MUL: u64 = 0x2d35_8dcc_aa6c_78a5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_insert_and_get() {
        let mut table = HashTable::<u64>::with_capacity(16);

        // Insert a few keys
        let keys = [0x1234567890abcdef_u64, 0x9876543210fedcba_u64, 0xdeadbeefcafebabe_u64];

        for &key in &keys {
            table.insert(key, key + 1000);
        }

        // Verify all keys can be found
        for &key in &keys {
            let found = table.get(&key);
            assert!(found.is_some(), "Key {:#x} should be found in table", key);
            assert_eq!(*found.unwrap(), key + 1000, "Value should match for key {:#x}", key);
        }
    }
}