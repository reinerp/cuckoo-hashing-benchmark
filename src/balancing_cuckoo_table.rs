//! A cuckoo hash table with 2 choices of group, each with 8-16 buckets per group.

use std::hint::{black_box, likely};
use std::{alloc::Layout, ptr::NonNull};

use crate::TRACK_PROBE_LENGTH;
use crate::control::{Group, Tag, TagSliceExt as _};
use crate::u64_fold_hash_fast::{self, fold_hash_fast};
use crate::uunwrap::UUnwrap;

pub struct HashTable<V> {
    // Mask to get an index from a hash value. The value is one less than the
    // number of buckets in the table.
    bucket_mask: usize,

    aligned_bucket_mask: usize,

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
        let size = ctrl_offset + num_buckets;
        let layout = Layout::from_size_align(size, align).uunwrap();
        // Allocate
        let alloc = unsafe { std::alloc::alloc(layout) };
        // Write control
        let ctrl = unsafe { NonNull::new_unchecked(alloc.add(ctrl_offset)) };
        let ctrl_slice = unsafe { std::slice::from_raw_parts_mut(ctrl.as_ptr() as *mut Tag, num_buckets) };
        ctrl_slice.fill_empty();
        // dbg!(num_buckets, bucket_size, align, ctrl_offset, size, layout, alloc, ctrl);
        let seed = fastrand::Rng::with_seed(123).u64(..);
        let bucket_mask = num_buckets - 1;
        let aligned_bucket_mask = num_buckets - Group::WIDTH;

        Self {
            bucket_mask,
            aligned_bucket_mask,
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

    pub fn avg_probe_length(&self) -> f64 {
        self.total_probe_length as f64 / self.items as f64
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
        let pos0 = hash0 as usize & self.aligned_bucket_mask;
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
        let pos1 = hash1 as usize & self.aligned_bucket_mask;
        let group1 = unsafe { Group::load(self.ctrl(pos1)) };

        for bit in group1.match_tag(tag_hash) {
            let index = (pos1 + bit) & self.bucket_mask;

            let bucket = unsafe { self.bucket(index) };

            if unsafe { (*bucket).0 } == key {
                unsafe { (*bucket).1 = value };
                return (false, index);
            }
        }

        // No match. Now check both groups for an empty slot.
        let empty_pos0 = group0.match_empty().trailing_zeros();
        let empty_pos1 = group1.match_empty().trailing_zeros();
        let mut probe_length = 2;
        let mut insert_slot = None;
        if empty_pos1 > (Group::WIDTH - 3) && empty_pos1 < empty_pos0 {
            insert_slot = Some((pos1 + empty_pos1));
            probe_length = 2;
        } else if empty_pos0 < Group::WIDTH{
            insert_slot = Some((pos0 + empty_pos0));
            probe_length = 1;
        }
        if let Some(insert_slot) = insert_slot {
            let insert_slot = insert_slot & self.bucket_mask;
            unsafe { 
                self.set_ctrl(insert_slot, tag_hash);
                self.bucket(insert_slot).write((key, value));
                self.items += 1;
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += probe_length;
                    self.total_insert_probe_length += 2;
                    self.max_insert_probe_length = self.max_insert_probe_length.max(2);
                }
                return (true, insert_slot);
            }
        }

        // key is going to get inserted in the second location.
        if TRACK_PROBE_LENGTH {
            self.total_probe_length += 2;
        }
        let mut insert_probe_length = 1;

        // Cuckoo loop. Loop entry invariant: (key, value, hash) has no space in prev_group and should be tried in group `hash`.
        let mut key = key;
        let mut value = value;
        let mut hash = hash1;
        let mut tag_hash = tag_hash;
        loop {
            let pos = hash as usize & self.aligned_bucket_mask;
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
            tag_hash = std::mem::replace(unsafe { &mut *self.ctrl(pos + evict_index) }, tag_hash);
            hash = fold_hash_fast(key, self.seed);
            if hash as usize & self.aligned_bucket_mask == pos {
                // We evict from its first location and move to its second location.
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length += 1;
                }
                hash = hash.rotate_left(32);
            } else {
                // We evict from its second location and move to its first location.
                if TRACK_PROBE_LENGTH {
                    self.total_probe_length -= 1;
                }
            }
            insert_probe_length += 1;
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
            let pos = hash64 as usize & self.aligned_bucket_mask;
            let group = unsafe { Group::load(self.ctrl(pos)) };
            for bit in group.match_tag(tag_hash) {
                let index = (pos + bit) & self.bucket_mask;

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
            if (ALLOW_EARLY_RETURN && likely(group.match_empty().any_bit_set())) || is_second_group {
                return None;
            }
            hash64 = hash64.rotate_left(32);
            is_second_group = true;
        }
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
        *self.ctrl(index) = tag;
    }
}