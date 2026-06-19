//! A *linear* probing hash table for u64 keys, Indirect-SIMD (SwissTable) layout.
//!
//! Identical to `quadratic_probing_table` except for the probe sequence: instead of triangular
//! (quadratic) stepping, we step by exactly one SIMD group (`Group::WIDTH` tags) each probe. This
//! is the simplest possible probe sequence: `pos = (pos + WIDTH) & mask`, with no running `stride`
//! register.
//!
//! Correctness of full coverage: the table size is a power of two and a multiple of `Group::WIDTH`.
//! Starting from an arbitrary (unaligned) `pos` and stepping by `WIDTH` visits exactly the
//! `num_buckets / WIDTH` windows congruent to `pos (mod WIDTH)`, which tile the whole tag array
//! (adjacent, non-overlapping), so every slot is covered exactly once per cycle. The replicated
//! `Group::WIDTH` tail control bytes make the wrap-around group load valid, exactly as in the
//! quadratic table.

use std::hint::likely;
use std::{alloc::Layout, ptr::NonNull};

use crate::TRACK_PROBE_LENGTH;
use crate::control::{Group, Tag, TagSliceExt as _};
use crate::u64_fold_hash_fast::fold_hash_fast;
use crate::uunwrap::UUnwrap;
use crate::dropper::Dropper;

pub struct HashTable<V> {
    bucket_mask: usize,
    ctrl: NonNull<u8>,
    items: usize,
    seed: u64,
    marker: std::marker::PhantomData<V>,
    total_probe_length: usize,
    dropper: Dropper,
}

/// Linear probe sequence: step by one group (`Group::WIDTH`) each probe.
#[derive(Clone)]
struct ProbeSeq {
    pos: usize,
}

impl ProbeSeq {
    #[inline]
    fn move_next(&mut self, bucket_mask: usize) {
        self.pos = (self.pos + Group::WIDTH) & bucket_mask;
    }
}

impl<V> HashTable<V> {
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let num_buckets = ((capacity * 8) / 7).next_power_of_two();
        let bucket_size = std::mem::size_of::<(u64, V)>();
        let align = std::mem::align_of::<(u64, V)>().max(Group::WIDTH);
        let ctrl_offset = (bucket_size * num_buckets).next_multiple_of(align);
        let size = ctrl_offset + num_buckets + Group::WIDTH;
        let layout = Layout::from_size_align(size, align).uunwrap();
        let alloc = unsafe { std::alloc::alloc(layout) };
        let ctrl = unsafe { NonNull::new_unchecked(alloc.add(ctrl_offset)) };
        let ctrl_slice = unsafe {
            std::slice::from_raw_parts_mut(ctrl.as_ptr() as *mut Tag, num_buckets + Group::WIDTH)
        };
        ctrl_slice.fill_empty();
        let seed = fastrand::Rng::with_seed(123).u64(..);

        Self {
            bucket_mask: num_buckets - 1,
            ctrl,
            items: 0,
            seed,
            marker: std::marker::PhantomData,
            total_probe_length: 0,
            dropper: Dropper { alloc, layout },
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.items
    }

    pub fn print_stats(&self) {
        println!(
            "  avg_probe_length: {}",
            self.total_probe_length as f64 / self.items as f64
        );
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, value: V) -> (bool, usize, usize) {
        let mut insert_slot = None;
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);

        let mut probe_seq = self.probe_seq(hash64);
        let mut insertion_probe_length = 1;

        loop {
            let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };

            for bit in group.match_tag(tag_hash) {
                let index = (probe_seq.pos + bit) & self.bucket_mask;
                let bucket = unsafe { self.bucket(index) };
                if unsafe { (*bucket).0 } == key {
                    unsafe { (*bucket).1 = value };
                    return (false, index, insertion_probe_length);
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
                        self.set_ctrl(insert_slot, tag_hash);
                        self.bucket(insert_slot).write((key, value));
                        self.items += 1;
                        if TRACK_PROBE_LENGTH {
                            self.total_probe_length += insertion_probe_length;
                        }
                        return (true, insert_slot, insertion_probe_length);
                    }
                }
            }

            probe_seq.move_next(self.bucket_mask);
            insertion_probe_length += 1;
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
                if likely(unsafe { (*bucket).0 } == key) {
                    return Some(unsafe { &(*bucket).1 });
                }
            }

            if likely(group.match_empty().any_bit_set()) {
                return None;
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    pub fn probe_length(&self, key: u64) -> (usize, bool) {
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let mut probe_seq = self.probe_seq(hash64);
        let mut probe_count = 0;

        loop {
            probe_count += 1;
            let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };

            for bit in group.match_tag(tag_hash) {
                let index = (probe_seq.pos + bit) & self.bucket_mask;
                let bucket = unsafe { self.bucket(index) };
                if unsafe { (*bucket).0 } == key {
                    return (probe_count, true);
                }
            }

            if group.match_empty().any_bit_set() {
                return (probe_count, false);
            }

            probe_seq.move_next(self.bucket_mask);
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

    /// Backward-shift deletion (Knuth Algorithm R), tombstone-free. Removes the element at `index`
    /// and shifts back any following elements whose probe path runs through the freed slot, so the
    /// table stays gap-free (every key still sits contiguously from its home). This is the deletion
    /// quadratic probing *cannot* do (its non-contiguous probe sequence forces tombstones), and it
    /// keeps probe lengths from degrading under churn without ever rebuilding.
    ///
    /// Correctness rests on this table being logically slot-level linear probing: an element with
    /// home `h` stored at slot `s` has all of `[h, s)` full, so `get` (which stops at the first
    /// empty slot in any window) finds it. Shifting preserves that invariant.
    #[inline]
    pub unsafe fn remove_at(&mut self, index: usize) {
        let mask = self.bucket_mask;
        let mut hole = index;
        let mut j = index;
        loop {
            j = (j + 1) & mask;
            let tag = *self.ctrl(j);
            if tag == Tag::EMPTY {
                break;
            }
            let key_j = (*self.bucket(j)).0;
            let home = (fold_hash_fast(key_j, self.seed) as usize) & mask;
            // Move element j back into the hole iff the hole lies on j's probe path, i.e. going
            // forward from `home` we reach `hole` no later than `j`:
            //   dist(home -> j) >= dist(hole -> j)   (both mod table size).
            let dist_home = j.wrapping_sub(home) & mask;
            let dist_hole = j.wrapping_sub(hole) & mask;
            if dist_home >= dist_hole {
                let kv = self.bucket(j).read();
                self.bucket(hole).write(kv);
                self.set_ctrl(hole, tag);
                hole = j;
            }
            // else: element j is already in place relative to the hole; leave it, keep scanning.
        }
        self.set_ctrl(hole, Tag::EMPTY);
        self.items -= 1;
    }

    /// Find `key` and remove it via backward-shift deletion. Returns whether it was present.
    pub fn remove(&mut self, key: u64) -> bool {
        let hash64 = fold_hash_fast(key, self.seed);
        let tag_hash = Tag::full(hash64);
        let mut probe_seq = self.probe_seq(hash64);
        loop {
            let group = unsafe { Group::load(self.ctrl(probe_seq.pos)) };
            for bit in group.match_tag(tag_hash) {
                let index = (probe_seq.pos + bit) & self.bucket_mask;
                if unsafe { (*self.bucket(index)).0 } == key {
                    unsafe { self.remove_at(index) };
                    return true;
                }
            }
            if group.match_empty().any_bit_set() {
                return false;
            }
            probe_seq.move_next(self.bucket_mask);
        }
    }

    #[inline(always)]
    pub unsafe fn insert_and_erase(&mut self, key: u64, value: V) {
        let (inserted, index, _) = self.insert(key, value);
        if inserted {
            // Tombstone-free backward-shift deletion (NOT a set-EMPTY shortcut): charges the real
            // deletion cost of linear probing.
            self.remove_at(index);
        }
    }

    #[cfg(test)]
    pub fn count_deleted(&self) -> usize {
        let n = self.bucket_mask + 1;
        (0..n)
            .filter(|&i| unsafe { *self.ctrl(i) } == Tag::DELETED)
            .count()
    }

    #[cfg(test)]
    pub fn num_buckets(&self) -> usize {
        self.bucket_mask + 1
    }

    #[cfg(test)]
    pub fn seed_for_test(&self) -> u64 {
        self.seed
    }

    #[cfg(test)]
    pub fn ctrl_at(&self, i: usize) -> u8 {
        unsafe { (*self.ctrl(i)).0 }
    }

    /// Reads the raw contiguous byte at primary position `pos + off`. For `pos` near the wrap this
    /// reads into the replicated tail mirror, exactly as a SIMD `Group::load` does.
    #[cfg(test)]
    pub fn window_byte(&self, pos: usize, off: usize) -> u8 {
        unsafe { *self.ctrl.as_ptr().add(pos + off) }
    }

    /// Slots `[0, WIDTH)` are mirrored into the tail `[m, m+WIDTH)`. Verify each primary equals its
    /// tail mirror; returns the first inconsistency as Err.
    #[cfg(test)]
    pub fn check_mirror(&self) -> Result<(), String> {
        let m = self.bucket_mask + 1;
        for i in 0..Group::WIDTH {
            let primary = unsafe { (*self.ctrl(i)).0 };
            let mirror = unsafe { (*self.ctrl(m + i)).0 };
            if primary != mirror {
                return Err(format!(
                    "mirror mismatch at i={i}: primary ctrl[{i}]={primary:#04x} mirror ctrl[{}]={mirror:#04x}",
                    m + i
                ));
            }
        }
        Ok(())
    }

    /// Independent (non-SIMD) slot-level linear scan: walk forward from `home`, stop at the first
    /// EMPTY, return the slot holding `key` if found first. Mirrors `get()`'s logical semantics.
    #[cfg(test)]
    pub fn find_slot_scalar(&self, key: u64) -> Option<usize> {
        let m = self.bucket_mask + 1;
        let home = (fold_hash_fast(key, self.seed) as usize) & self.bucket_mask;
        for d in 0..m {
            let slot = (home + d) & self.bucket_mask;
            let tag = unsafe { (*self.ctrl(slot)).0 };
            if tag == Tag::EMPTY.0 {
                return None;
            }
            if unsafe { (*self.bucket(slot)).0 } == key {
                return Some(slot);
            }
        }
        None
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

    fn probe_seq(&self, hash64: u64) -> ProbeSeq {
        ProbeSeq {
            pos: (hash64 as usize) & self.bucket_mask,
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
            // These tables are fixed-capacity (no auto-grow), so size for the insert count.
            // with_capacity(3000) -> 4096 slots -> ~0.73 load with 3000 distinct keys.
            let mut table = HashTable::<u64>::with_capacity(3000);
            let mut std_map: HashMap<u64, u64> = HashMap::new();
            // Fill to a high load to exercise long linear runs and wrap-around.
            for _ in 0..3000 {
                let key = rng.u64(..);
                let val = rng.u64(..);
                let (_inserted, _, _) = table.insert(key, val);
                std_map.insert(key, val);
            }
            assert_eq!(table.len(), std_map.len(), "len mismatch seed={seed}");
            // Every present key must be found with the right value.
            for (&k, &v) in &std_map {
                assert_eq!(table.get(&k).copied(), Some(v), "missing key {k:x} seed={seed}");
            }
            // Absent keys must not be found.
            for _ in 0..3000 {
                let k = rng.u64(..);
                assert_eq!(table.get(&k).copied(), std_map.get(&k).copied(), "absent mismatch {k:x}");
            }
        }
    }

    #[test]
    fn backward_shift_deletion_cross_check() {
        // Interleave random inserts and deletes against std::HashMap, with a small key space so
        // clusters, run-overlap, and wrap-around are heavily exercised. After every op, the table
        // must agree with the reference, and NO tombstones may ever appear (backward-shift is
        // tombstone-free). A final full lookup sweep confirms nothing was made unreachable.
        for seed in [1u64, 2, 7, 42, 2024] {
            let mut rng = fastrand::Rng::with_seed(seed);
            // 8192 slots; key space 1..7000 -> up to ~85% load. Key 0 avoided (insert is generic).
            let mut table = HashTable::<u64>::with_capacity(4000);
            let mut std_map: HashMap<u64, u64> = HashMap::new();
            for step in 0..60_000u32 {
                // 60% insert, 40% delete -> oscillates around a high load.
                if rng.u32(0..5) < 3 {
                    let k = rng.u64(1..7000);
                    let v = rng.u64(..);
                    table.insert(k, v);
                    std_map.insert(k, v);
                } else {
                    let k = rng.u64(1..7000);
                    let removed = table.remove(k);
                    let std_removed = std_map.remove(&k).is_some();
                    assert_eq!(removed, std_removed, "remove mismatch key={k} step={step} seed={seed}");
                }
                assert_eq!(table.count_deleted(), 0, "tombstone appeared! step={step} seed={seed}");
                assert_eq!(table.len(), std_map.len(), "len mismatch step={step} seed={seed}");
                // Spot-check a couple of keys each step (full sweep at the end).
                if step % 997 == 0 {
                    for (&k, &v) in std_map.iter().take(8) {
                        assert_eq!(table.get(&k).copied(), Some(v), "lost key {k} step={step} seed={seed}");
                    }
                }
            }
            // Final exhaustive agreement check.
            for (&k, &v) in &std_map {
                assert_eq!(table.get(&k).copied(), Some(v), "final missing {k} seed={seed}");
            }
            for k in 1..7000u64 {
                assert_eq!(table.get(&k).copied(), std_map.get(&k).copied(), "final absent {k} seed={seed}");
            }
        }
    }

    // Verify the raw contiguous window bytes (primary array + replicated tail) match the masked
    // primaries for EVERY tail-straddling window. This is exactly what a SIMD `Group::load` reads
    // near the wrap. If the mirror is ever left stale by remove_at, a straddling window sees a byte
    // that disagrees with the true (masked) tag at that slot.
    fn assert_tail_windows_consistent(table: &HashTable<u64>, ctx: &str) {
        let m = table.num_buckets();
        let w = Group::WIDTH;
        // pos from m-w+1 .. m are the windows that read into the tail.
        for pos in (m - w + 1)..m {
            for off in 0..w {
                let raw = table.window_byte(pos, off);
                let logical = (pos + off) & (m - 1);
                let masked = table.ctrl_at(logical);
                assert_eq!(
                    raw, masked,
                    "tail window byte mismatch {ctx}: pos={pos} off={off} logical={logical} raw={raw:#04x} masked={masked:#04x}"
                );
            }
        }
    }

    fn full_invariants(table: &HashTable<u64>, std_map: &HashMap<u64, u64>, ctx: &str) {
        assert_eq!(table.count_deleted(), 0, "tombstone {ctx}");
        assert_eq!(table.len(), std_map.len(), "len {ctx}");
        table.check_mirror().unwrap_or_else(|e| panic!("{ctx}: {e}"));
        assert_tail_windows_consistent(table, ctx);
    }

    // Brutal small-table churn: tiny tables maximize wrap-around and cluster straddle across slot 0.
    // After EVERY op we check: full std agreement (every key via BOTH SIMD get() and an independent
    // scalar scan), zero tombstones, full mirror consistency, and tail-window byte consistency.
    #[test]
    fn backward_shift_wraparound_mirror_stress() {
        // Several tiny capacities; with_capacity rounds up*8/7 to next pow2.
        for &cap in &[8usize, 12, 24, 50] {
            let m = ((cap * 8) / 7).next_power_of_two();
            for seed in 0..40u64 {
                let mut rng = fastrand::Rng::with_seed(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (cap as u64));
                let mut table = HashTable::<u64>::with_capacity(cap);
                let mut std_map: HashMap<u64, u64> = HashMap::new();
                // Small key space relative to slots -> high load, dense clusters.
                let key_hi = (m as u64) * 7 / 8; // stay under max load capacity overall isn't enforced per-key but keeps it dense
                let key_hi = key_hi.max(4);
                for step in 0..4000u32 {
                    let ctx = format!("cap={cap} m={m} seed={seed} step={step}");
                    if rng.u32(0..2) == 0 {
                        // insert (only if room: table is fixed-capacity, must not exceed ~87.5%)
                        if table.len() < (m * 7) / 8 {
                            let k = rng.u64(1..=key_hi);
                            let v = rng.u64(..);
                            table.insert(k, v);
                            std_map.insert(k, v);
                        }
                    } else {
                        let k = rng.u64(1..=key_hi);
                        let removed = table.remove(k);
                        let std_removed = std_map.remove(&k).is_some();
                        assert_eq!(removed, std_removed, "remove mismatch {ctx} key={k}");
                    }
                    full_invariants(&table, &std_map, &ctx);
                    // Full agreement EVERY step (tables are tiny).
                    for (&k, &v) in &std_map {
                        assert_eq!(table.get(&k).copied(), Some(v), "get lost {k} {ctx}");
                        assert_eq!(table.find_slot_scalar(k).is_some(), true, "scalar lost {k} {ctx}");
                    }
                    for k in 1..=key_hi {
                        let want = std_map.get(&k).copied();
                        assert_eq!(table.get(&k).copied(), want, "get absent-disagree {k} {ctx}");
                        let scalar_present = table.find_slot_scalar(k).is_some();
                        assert_eq!(scalar_present, want.is_some(), "scalar absent-disagree {k} {ctx}");
                    }
                }
            }
        }
    }

    // Deterministically build a cluster that STRADDLES the wrap (slot 0), then delete from its
    // middle and confirm both the SIMD path and the scalar path still find every survivor, and the
    // mirror/tail stay consistent. We discover keys whose home is the last slot m-1 (or m-2) by
    // brute force so the run is forced to wrap into slots 0,1,...
    #[test]
    fn wrap_straddle_targeted() {
        let cap = 12usize;
        let mut table = HashTable::<u64>::with_capacity(cap);
        let m = table.num_buckets();
        // Collect, for each home slot, a few keys mapping there.
        // Build a run starting at home = m-2 so it spills over the wrap: m-2, m-1, 0, 1, ...
        let target_home = m - 2;
        let mut chain_keys: Vec<u64> = Vec::new();
        let mut k = 1u64;
        // We want enough keys whose home is target_home OR earlier-in-run homes so the run is dense
        // across the wrap. Simplest: many keys with the SAME home target_home; linear probing then
        // lays them out contiguously m-2, m-1, 0, 1, 2, ... straddling the wrap.
        while chain_keys.len() < 6 && k < 5_000_000 {
            let home = (fold_hash_fast(k, table.seed_for_test()) as usize) & (m - 1);
            if home == target_home {
                chain_keys.push(k);
            }
            k += 1;
        }
        assert!(chain_keys.len() >= 6, "could not synthesize wrap-straddling chain (found {})", chain_keys.len());
        let mut std_map: HashMap<u64, u64> = HashMap::new();
        for (i, &key) in chain_keys.iter().enumerate() {
            table.insert(key, i as u64);
            std_map.insert(key, i as u64);
        }
        // Sanity: the chain should occupy slots straddling 0.
        full_invariants(&table, &std_map, "wrap build");
        // Now delete from the MIDDLE of the straddling run repeatedly (the worst case for
        // backward-shift across the wrap), each time re-checking everything.
        let order = [2usize, 0, 4, 1, 3, 5];
        for &idx in &order {
            if idx >= chain_keys.len() { continue; }
            let key = chain_keys[idx];
            if std_map.contains_key(&key) {
                let removed = table.remove(key);
                assert!(removed, "expected to remove chain key {key}");
                std_map.remove(&key);
            }
            let ctx = format!("wrap delete idx={idx} key={key}");
            full_invariants(&table, &std_map, &ctx);
            for (&kk, &vv) in &std_map {
                assert_eq!(table.get(&kk).copied(), Some(vv), "get survivor {kk} {ctx}");
                assert!(table.find_slot_scalar(kk).is_some(), "scalar survivor {kk} {ctx}");
            }
        }
    }

    // ---- SKEPTIC 3: invariant vs window-step ----
    // The existing tests confirm get()==std and mirror/tail consistency. This block adds the ONE
    // thing they don't assert *directly*: the slot-level contiguity invariant
    //   "for every present key with home h stored at slot s, every slot in [h, s) is non-EMPTY"
    // which is exactly the property a window-step lookup (stop at first EMPTY window) depends on.
    // A violation is, by definition, an EMPTY in an earlier window than the key's storage window.

    /// Returns Err((key, home, stored_slot, empty_slot)) on the first contiguity violation:
    /// a present key for which some slot strictly between its home and its storage slot is EMPTY.
    fn check_contiguity(table: &HashTable<u64>) -> Result<(), (u64, usize, usize, usize)> {
        let m = table.num_buckets();
        for s in 0..m {
            let tag = table.ctrl_at(s);
            if tag == Tag::EMPTY.0 || tag == Tag::DELETED.0 {
                continue; // not a stored key
            }
            // stored key at s
            let key = unsafe { (*table.bucket(s)).0 };
            let home = (fold_hash_fast(key, table.seed_for_test()) as usize) & (m - 1);
            let mut p = home;
            while p != s {
                if table.ctrl_at(p) == Tag::EMPTY.0 {
                    return Err((key, home, s, p));
                }
                p = (p + 1) & (m - 1);
            }
        }
        Ok(())
    }

    fn assert_contiguity(table: &HashTable<u64>, ctx: &str) {
        if let Err((key, home, s, p)) = check_contiguity(table) {
            panic!(
                "CONTIGUITY VIOLATED {ctx}: key={key} home={home} stored_at={s} EMPTY_at={p} \
                 (EMPTY in [home, stored) => earlier-window get() would return None)"
            );
        }
    }

    // Adversarial: pile keys onto a HANDFUL of homes that all fall inside a single WIDTH-window, so
    // probe windows for those homes overlap maximally. Then churn (insert/delete) and after EVERY op
    // assert the contiguity invariant directly, plus windowed get() == std for the whole hot set.
    // If backward-shift could ever push a present key past h+WIDTH while leaving an EMPTY inside
    // [h, h+WIDTH), check_contiguity would catch it (and get() would diverge from std).
    #[test]
    fn skeptic3_overlapping_window_homes() {
        let w = Group::WIDTH;
        for &cap in &[12usize, 24, 50] {
            let m = ((cap * 8) / 7).next_power_of_two();
            for seed in 0..30u64 {
                let mut table = HashTable::<u64>::with_capacity(cap);
                let mut rng = fastrand::Rng::with_seed(0xABCDEFu64 ^ seed ^ ((cap as u64) << 32));

                // Choose a base home; gather keys whose home is in [base, base+w) so all their
                // windows overlap. Brute-force a pool of such keys.
                let base = rng.usize(0..m) & (m - 1);
                let in_window = |home: usize| -> bool {
                    let d = home.wrapping_sub(base) & (m - 1);
                    d < w
                };
                let mut hot: Vec<u64> = Vec::new();
                let mut k = 1u64;
                while hot.len() < (m * 7) / 8 && k < 3_000_000 {
                    let home = (fold_hash_fast(k, table.seed_for_test()) as usize) & (m - 1);
                    if in_window(home) {
                        hot.push(k);
                    }
                    k += 1;
                }
                if hot.len() < 3 {
                    continue;
                }

                let mut std_map: HashMap<u64, u64> = HashMap::new();
                for step in 0..3000u32 {
                    let ctx = format!("cap={cap} m={m} seed={seed} step={step} base={base}");
                    let key = hot[rng.usize(0..hot.len())];
                    if rng.bool() && table.len() < (m * 7) / 8 {
                        let v = rng.u64(..);
                        table.insert(key, v);
                        std_map.insert(key, v);
                    } else {
                        let removed = table.remove(key);
                        let std_removed = std_map.remove(&key).is_some();
                        assert_eq!(removed, std_removed, "remove mismatch {ctx} key={key}");
                    }

                    assert_eq!(table.count_deleted(), 0, "tombstone {ctx}");
                    assert_contiguity(&table, &ctx);
                    table.check_mirror().unwrap_or_else(|e| panic!("{ctx}: {e}"));

                    // Windowed get() must agree with std for the whole overlapping-home set.
                    for &kk in &hot {
                        let want = std_map.contains_key(&kk);
                        assert_eq!(
                            table.get(&kk).is_some(),
                            want,
                            "windowed get disagrees with std for key={kk} {ctx}"
                        );
                        // And with an independent slot-level scan.
                        assert_eq!(
                            table.find_slot_scalar(kk).is_some(),
                            want,
                            "scalar get disagrees with std for key={kk} {ctx}"
                        );
                    }
                }
            }
        }
    }

    // Adversarial: build a SINGLE contiguous run longer than WIDTH (so it spans multiple overlapping
    // windows), by piling keys with the same home. Then delete in random order — including the front
    // slot (== home), which forces the longest backward-shift cascade ACROSS window boundaries.
    // After each deletion, every surviving key (some stored at slot >= home+WIDTH) must still be
    // found by the WINDOWED get(); a single failure is the window-step counterexample the skeptic
    // asks for.
    #[test]
    fn skeptic3_long_run_crosses_window_boundary() {
        let w = Group::WIDTH;
        for seed in 0..50u64 {
            let mut table = HashTable::<u64>::with_capacity(300);
            let m = table.num_buckets();
            let mut rng = fastrand::Rng::with_seed(0x1234_5678u64 ^ seed);

            // Find a home and (2*w + 5) keys all mapping to it -> a run of that length that
            // definitely crosses at least one window boundary (run length > w).
            let want_len = 2 * w + 5;
            let target_home = rng.usize(0..m) & (m - 1);
            let mut run_keys: Vec<u64> = Vec::new();
            let mut k = 1u64;
            while run_keys.len() < want_len && k < 8_000_000 {
                let home = (fold_hash_fast(k, table.seed_for_test()) as usize) & (m - 1);
                if home == target_home {
                    run_keys.push(k);
                }
                k += 1;
            }
            assert!(
                run_keys.len() == want_len,
                "could not synthesize run of {want_len} same-home keys (got {}) seed={seed}",
                run_keys.len()
            );

            let mut std_map: HashMap<u64, u64> = HashMap::new();
            for (i, &key) in run_keys.iter().enumerate() {
                table.insert(key, i as u64);
                std_map.insert(key, i as u64);
            }
            assert_contiguity(&table, &format!("build seed={seed}"));
            // Confirm the run really does extend past home+WIDTH (i.e. windows overlap & a key lives
            // beyond the first window).
            let mut max_dist = 0usize;
            for &key in &run_keys {
                let s = table.find_slot_scalar(key).expect("present");
                let d = s.wrapping_sub(target_home) & (m - 1);
                max_dist = max_dist.max(d);
            }
            assert!(max_dist >= w, "run did not cross a window boundary (max_dist={max_dist}, w={w}) seed={seed}");

            // Delete in random order, re-checking after each removal.
            let mut alive: Vec<u64> = run_keys.clone();
            while !alive.is_empty() {
                let victim = alive.swap_remove(rng.usize(0..alive.len()));
                assert!(table.remove(victim), "remove {victim} seed={seed}");
                std_map.remove(&victim);
                let ctx = format!("after remove {victim} seed={seed} (home={target_home})");

                assert_eq!(table.count_deleted(), 0, "tombstone {ctx}");
                assert_contiguity(&table, &ctx);
                table.check_mirror().unwrap_or_else(|e| panic!("{ctx}: {e}"));

                for &kk in &alive {
                    assert!(
                        table.get(&kk).is_some(),
                        "WINDOW-STEP COUNTEREXAMPLE: key={kk} lost by windowed get() {ctx}"
                    );
                    assert!(table.find_slot_scalar(kk).is_some(), "scalar lost key={kk} {ctx}");
                    assert_eq!(std_map.get(&kk).copied(), table.get(&kk).copied(), "value mismatch {kk} {ctx}");
                }
            }
        }
    }
}
