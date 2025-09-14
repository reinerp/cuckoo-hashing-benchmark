//! A quadratic probing hash table for u64 keys. SwissTable design following `hashbrown` crate,
//! with a lot of features removed but the same optimizations valid.

use std::{alloc::Layout, ptr::NonNull};

use crate::control::{Group, Tag, TagSliceExt as _};
use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::uunwrap::UUnwrap;
use crate::TRACK_PROBE_LENGTH;

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

    total_probe_length: usize,

    marker: std::marker::PhantomData<V>,
}

/// Probe sequence based on triangular numbers, which is guaranteed (since our
/// table size is a power of two) to visit every group of elements exactly once.
///
/// A triangular probe has us jump by 1 more group every time. So first we
/// jump by 1 group (meaning we just continue our linear scan), then 2 groups
/// (skipping over 1 group), then 3 groups (skipping over 2 groups), and so on.
///
/// Proof that the probe will visit every group in the table:
/// <https://fgiesen.wordpress.com/2015/02/22/triangular-numbers-mod-2n/>
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

        self.pos += self.stride;
        self.pos &= bucket_mask;
    }
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
        let ctrl_slice =
            unsafe { std::slice::from_raw_parts_mut(ctrl.as_ptr() as *mut Tag, num_buckets) };
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
            total_probe_length: 0,
        }
    }

    pub fn print_stats(&self) {
        println!(
            "  avg_probe_length: {}",
            self.total_probe_length as f64 / self.items as f64
        );
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.items
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, usize) {
        let mut insert_slot = None;
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);

        let mut probe_seq = self.probe_seq(hash64);

        loop {
            let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };
            if TRACK_PROBE_LENGTH {
                self.total_probe_length += 1;
            }

            for bit in group.match_tag(tag_hash) {
                let index = (probe_seq.pos + bit) & self.bucket_mask;

                let bucket = unsafe { self.bucket(index) };

                if unsafe { (*bucket).0 } == key {
                    unsafe { (*bucket).1 = value };
                    return (false, index);
                }
            }

            if insert_slot.is_none() {
                insert_slot = group
                    .match_empty_or_deleted()
                    .lowest_set_bit()
                    .map(|bit| probe_seq.pos + bit);
            }

            if let Some(insert_slot) = insert_slot {
                if group.match_empty().any_bit_set() {
                    let insert_slot = insert_slot & self.bucket_mask;
                    unsafe {
                        // The first Group::WIDTH control slots are replicated as the last Group::WIDTH control slots. We
                        // write to both.
                        self.set_ctrl(insert_slot, tag_hash);
                        self.bucket(insert_slot).write((key, value));
                        self.items += 1;
                        return (true, insert_slot);
                    }
                }
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    #[inline(always)]
    pub fn get(&mut self, key: &u64) -> Option<&V> {
        let key = *key;
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);

        let mut probe_seq = self.probe_seq(hash64);
        loop {
            let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };

            for bit in group.match_tag(tag_hash) {
                let index = (probe_seq.pos + bit) & self.bucket_mask;

                let bucket = unsafe { self.bucket(index) };

                if unsafe { (*bucket).0 } == key {
                    return Some(unsafe { &(*bucket).1 });
                }
            }

            if group.match_empty().any_bit_set() {
                return None;
            }

            probe_seq.move_next(self.bucket_mask);
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

    fn probe_seq(&self, hash64: u64) -> ProbeSeq {
        ProbeSeq {
            pos: (hash64 as usize) & self.aligned_bucket_mask,
            stride: (hash64.rotate_left(32) as usize & self.aligned_bucket_mask) | Group::WIDTH,
        }
    }
}
