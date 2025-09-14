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

        // Cuckoo loop. Loop entry invariant: (key, value, hash) has no space in prev_group and should be tried in group `hash`.
        let mut key = key;
        let mut value = value;
        let mut hash = hash1;
        let mut tag_hash = tag_hash;
        loop {
            let pos = hash as usize & self.bucket_mask;
            let group = unsafe { Group::load(self.ctrl(pos)) };
            if let Some(insert_slot) = group.match_empty().lowest_set_bit() {
                let insert_slot = (pos + insert_slot) & self.bucket_mask;
                unsafe { 
                    self.set_ctrl(insert_slot, tag_hash);
                    self.bucket(insert_slot).write((key, value));
                    self.items += 1;
                    insert_probe_length += 1;
                    if TRACK_PROBE_LENGTH {
                        self.total_insert_probe_length += insert_probe_length;
                        self.max_insert_probe_length = self.max_insert_probe_length.max(insert_probe_length);
                    }
                    return (true, insert_slot);
                }
            }
            let evict_index = self.rng.usize(..) % Group::WIDTH;
            (key, value) = std::mem::replace(unsafe { &mut *self.bucket(pos + evict_index) }, (key, value));
            let new_tag_hash = unsafe { *self.ctrl(pos + evict_index) };
            unsafe { self.set_ctrl(pos + evict_index, tag_hash) };
            hash = fold_hash_fast(key, self.seed);
            tag_hash = new_tag_hash;
            let evicted_group_start = hash as usize & self.bucket_mask;
            if evict_index.wrapping_sub(evicted_group_start) & self.bucket_mask < Group::WIDTH {
                hash = hash.rotate_left(32);
                // We evict from its first location and move to its second location.
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += 1;
                }
            } else {
                // We evict from its second location and move to its first location.
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length -= 1;
                }
            }
            insert_probe_length += 1;
            // loop_count += 1;
            // if loop_count > 1000 {
            //     println!("Loop: key={key:x}, pos={pos:x}, hash={hash:x}");
            //     if loop_count > 120 {
            //         panic!();
            //     }
            // }
            // TODO: panic and rehash on loop.
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
            // TODO(reiner): possibly skip early return here. The early return prevents deletions.
            if is_second_group || group.match_empty().any_bit_set() {
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