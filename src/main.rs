#![allow(unused)]
#![allow(unsafe_op_in_unsafe_fn)]
#![feature(likely_unlikely)]
#![feature(rust_cold_cc)]
use std::{hint::black_box, io::Write, time::Instant};

mod aligned_cuckoo_table;
mod aligned_double_hashing_table;
mod aligned_quadratic_probing_table;
mod balancing_cuckoo_table;
mod control;
mod quadratic_probing_table;
mod scalar_cache_line_aligned_table;
mod scalar_cuckoo_table;
mod scalar_unaligned_table;
mod u64_fold_hash_fast;
mod unaligned_cuckoo_table;
mod uunwrap;
mod dropper;
mod direct_simd_cuckoo_table;
mod control64;
mod localized_simd_cuckoo_table;
mod direct_simd_quadratic_probing;
mod linear_probing_table;
mod direct_simd_linear_probing;
mod direct_simd_linear_probing_np2;

const ITERS: usize = 40_000_000;
const TRACK_PROBE_LENGTH: bool = false;
// Toggle which workloads run. The four lookup/churn ops are the main sweep; BENCH_BUILD adds an
// amortized build-from-empty measurement (insert n distinct keys into a pre-sized table).
const BENCH_OPS: bool = true;
const BENCH_BUILD: bool = true;
// Focus switch: when false, the find workloads are skipped (used to re-measure churn/build in
// isolation). Set true for the full sweep.
const RUN_FINDS: bool = true;

trait PrintStats {
    fn print_stats(&self) {}
}

impl PrintStats for hashbrown::HashMap<u64, u64> {}

trait InsertAndErase {
    fn insert_and_erase(&mut self, key: u64, value: u64) {}
}

impl InsertAndErase for hashbrown::HashMap<u64, u64> {
    fn insert_and_erase(&mut self, key: u64, value: u64) {
        self.entry(key).insert(value).remove();
    }
}

trait ProbeLength {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        (1, false) // Default dummy implementation
    }
}

impl ProbeLength for hashbrown::HashMap<u64, u64> {}
impl ProbeLength for aligned_double_hashing_table::HashTable<u64> {}
impl ProbeLength for aligned_quadratic_probing_table::HashTable<u64> {}
impl ProbeLength for balancing_cuckoo_table::HashTable<u64> {}
impl ProbeLength for scalar_cache_line_aligned_table::U64HashSet<u64> {}
impl ProbeLength for scalar_unaligned_table::U64HashSet<u64> {}
impl ProbeLength for scalar_cuckoo_table::U64HashSet<u64> {}
impl ProbeLength for localized_simd_cuckoo_table::HashTable<u64> {}

// Real implementations for tables that have proper probe_length methods
impl ProbeLength for aligned_cuckoo_table::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for unaligned_cuckoo_table::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for direct_simd_cuckoo_table::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for quadratic_probing_table::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for direct_simd_quadratic_probing::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for linear_probing_table::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for direct_simd_linear_probing::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

impl ProbeLength for direct_simd_linear_probing_np2::HashTable<u64> {
    fn probe_length(&self, key: u64) -> (usize, bool) {
        self.probe_length(key)
    }
}

fn drop_spaces(s: &str) -> String {
    s.split_whitespace().collect()
}

fn print_histogram(name: &str, histogram: &std::collections::HashMap<usize, usize>) {
    let total_probes: usize = histogram.iter()
        .map(|(length, count)| length * count)
        .sum();
    let total_count: usize = histogram.values().sum();
    let avg = if total_count > 0 {
        total_probes as f64 / total_count as f64
    } else { 0.0 };

    println!("  {} (avg: {:.3}):", name, avg);
    let mut keys: Vec<_> = histogram.keys().cloned().collect();
    keys.sort();
    for probe_length in keys {
        let count = histogram[&probe_length];
        println!("    {}: {}", probe_length, count);
    }
}

#[inline(always)]
fn mul_high_u64(x: u64, y: u64) -> u64 {
    let r = (x as u128) * (y as u128);
    (r >> 64) as u64
}

macro_rules! benchmark_find_miss {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            print!("find_miss  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let start = Instant::now();
            let mut found = 0;
            for _ in 0..ITERS {
                let key = rng.u64(..);
                found += table.get(&key).is_some() as usize;
            }
            black_box(found);
            let duration = start.elapsed();
            println!("{:.2} ns/op", duration.as_nanos() as f64 / ITERS as f64);
            if TRACK_PROBE_LENGTH {
                table.print_stats();
            }
        })
    };
}

macro_rules! benchmark_find_hit {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            print!("find_hit  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);
            let mut keys = (0..n).map(|i| i as u64).collect::<Vec<_>>();
            rng.shuffle(&mut keys);
            for key in keys {
                table.insert(key, <$v>::default());
            }
            let n_ish_mask = ((n.next_power_of_two() / 2) - 1) as u64;
            let start = Instant::now();
            let mut found = 0;
            for _ in 0..ITERS {
                let key = rng.u64(..) & n_ish_mask;
                found += table.get(&key).is_some() as usize;
            }
            black_box(found);
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / ITERS as f64
            );
        })
    };
}

macro_rules! benchmark_find_latency {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            print!(
                "find_hit_latency  {}/{n}: ",
                drop_spaces(stringify!($table))
            );
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let outer_iters = (ITERS / 3).div_ceil(n);
            let true_iters = outer_iters * n;
            let start = Instant::now();
            let mut found = 0;
            for _ in 0..outer_iters {
                let mut rng = fastrand::Rng::with_seed(123);
                let mut prev_value = 0;
                for _ in 0..n {
                    let key = rng.u64(..) ^ prev_value;
                    let Some(value) = table.get(&key) else {
                        panic!("key {key:x} not found");
                    };
                    prev_value = *value;
                }
                black_box(prev_value);
            }
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / true_iters as f64
            );
        })
    };
}

macro_rules! benchmark_insert_and_erase {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            print!("insert_erase  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let outer_iters = ITERS.div_ceil(n);
            let true_iters = outer_iters * n;
            let start = Instant::now();
            for _ in 0..outer_iters {
                let mut rng = fastrand::Rng::with_seed(456);
                for _ in 0..n {
                    let key = rng.u64(..);
                    unsafe { table.insert_and_erase(key, <$v>::default()) };
                }
            }
            black_box(table.len());
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / true_iters as f64
            );
        })
    };
}

macro_rules! benchmark_build_unreserved {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            _ = capacity;
            print!("build_unreserved  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let outer_iters = (ITERS / 8).div_ceil(n);
            let true_iters = outer_iters * n;
            let start = Instant::now();
            for _ in 0..outer_iters {
                let mut table = black_box(<$table>::new());
                let mut rng = fastrand::Rng::with_seed(124);
                for _ in 0..n {
                    let key = rng.u64(..);
                    table.insert(key, <$v>::default());
                }
                black_box(table.len());
            }
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / true_iters as f64
            );
        })
    };
}

macro_rules! benchmark_build_reserved {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            _ = capacity;
            print!("build_reserved  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let outer_iters = (ITERS / 8).div_ceil(n);
            let true_iters = outer_iters * n;
            let start = Instant::now();
            for _ in 0..outer_iters {
                let mut table = black_box(<$table>::with_capacity(capacity));
                let mut rng = fastrand::Rng::with_seed(124);
                for _ in 0..n {
                    let key = rng.u64(..);
                    table.insert(key, <$v>::default());
                }
                black_box(table.len());
            }
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / true_iters as f64
            );
        })
    };
}

macro_rules! benchmark_probe_histogram {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            println!("probe_histogram  {}/{n}:", drop_spaces(stringify!($table)));
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);

            // Insert keys same way as find_hit to get consistent results
            let mut keys = (0..n).map(|i| i as u64).collect::<Vec<_>>();
            rng.shuffle(&mut keys);
            for key in keys {
                table.insert(key, <$v>::default());
            }

            // Build histograms
            let mut present_histogram = std::collections::HashMap::new();
            let mut absent_histogram = std::collections::HashMap::new();

            let n_ish_mask = ((n.next_power_of_two() / 2) - 1) as u64;

            // Sample present keys
            for key in 0..n as u64 {
                let (probe_length, found) = table.probe_length(key);
                assert!(found);
                *present_histogram.entry(probe_length).or_insert(0) += 1;
            }

            // Sample absent keys
            let mut rng_absent = fastrand::Rng::with_seed(456);
            for _ in 0..n {
                let key = rng_absent.u64(..);
                let (probe_length, found) = table.probe_length(key);
                if !found {
                    *absent_histogram.entry(probe_length).or_insert(0) += 1;
                }
            }

            // Print histograms using shared function
            print_histogram("Present key probe lengths", &present_histogram);
            print_histogram("Absent key probe lengths", &absent_histogram);
        })
    };
}

macro_rules! benchmark_insertion_probe_histogram {
    ($table:ty, $v:ty) => {
        (|n: usize, capacity: usize| {
            println!("insertion_probe_histogram  {}/{n}:", drop_spaces(stringify!($table)));
            let mut table = <$table>::with_capacity(capacity);
            let mut rng = fastrand::Rng::with_seed(123);
            let mut insertion_histogram = std::collections::HashMap::new();

            // Insert keys and collect insertion probe lengths
            let mut keys = (0..n).map(|i| i as u64).collect::<Vec<_>>();
            rng.shuffle(&mut keys);
            for key in keys {
                let (_, _, insertion_probe_length) = table.insert(key, <$v>::default());
                *insertion_histogram.entry(insertion_probe_length).or_insert(0) += 1;
            }

            // Print histogram using shared function
            print_histogram("Insertion probe lengths", &insertion_histogram);
        })
    };
}

fn main() {
    // Head-to-head: LINEAR vs QUADRATIC vs CUCKOO probing, on two layouts (Indirect SIMD =
    // 1-byte tags + W=8 group; Direct SIMD = aligned [u64;4] cache-line buckets), across cache
    // residency (2^10 in-cache .. 2^25 far out-of-cache) and load factor (25% .. 87.5%).
    //
    // Output is line-per-benchmark "op  table/n: X.YZ ns/op", grouped under "mi:" and
    // "load factor:" headers, so it can be parsed mechanically.
    for lg_mi in [10usize, 15, 20, 25] {
        println!("mi: 2^{lg_mi}");
        let mi = 1usize << lg_mi;
        let in_cache = lg_mi <= 15; // run latency (branch-sensitive) only where it is meaningful
        for load_factor in [8usize, 12, 16, 20, 24, 28] {
            // 25%, 37.5%, 50%, 62.5%, 75%, 87.5%
            println!("load factor: {:.1}%", load_factor as f64 / 32.0 * 100.0);
            let n = (mi * load_factor / 32) - 1;
            let capacity = mi * 7 / 8;
            // Cuckoo insert+erase: now attempted at every load (87.5% included) to complete the
            // insertion picture; BFS should sustain it (k=2, group width 8 => capacity ~0.99).
            let cuckoo_insert_ok = true;

            // Full variant set per workload: quad family (unaligned/aligned indirect + direct),
            // linear (indirect + direct), cuckoo family (aligned/unaligned indirect + direct),
            // hashbrown reference. Best-of-layouts is taken per strategy in the analysis.
          if BENCH_OPS && RUN_FINDS {
            // ---------- FIND_MISS ----------
            benchmark_find_miss!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(linear_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(direct_simd_linear_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_miss!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);

            // ---------- FIND_HIT ----------
            benchmark_find_hit!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(linear_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(direct_simd_linear_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_find_hit!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);

            // ---------- FIND_HIT_LATENCY (in-cache only; memory-bound & non-discriminating OOC) ----------
            if in_cache {
                benchmark_find_latency!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(linear_probing_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(direct_simd_linear_probing::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                benchmark_find_latency!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);
            }
          } // BENCH_OPS && RUN_FINDS

          if BENCH_OPS {
            // ---------- INSERT_ERASE ----------  (linear = backward-shift; cuckoo = early-exit)
            benchmark_insert_and_erase!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_insert_and_erase!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_insert_and_erase!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_insert_and_erase!(linear_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_insert_and_erase!(direct_simd_linear_probing::HashTable::<u64>, u64)(n, capacity);
            if cuckoo_insert_ok {
                benchmark_insert_and_erase!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                // unaligned cuckoo insert+erase: cap at 75% (fixed-cap BFS, no growth headroom).
                if load_factor <= 24 {
                    benchmark_insert_and_erase!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                }
                benchmark_insert_and_erase!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            }
            benchmark_insert_and_erase!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);
          } // BENCH_OPS

          if BENCH_BUILD {
            // ---------- BUILD_RESERVED (amortized build-from-empty into a pre-sized table) ----------
            // Quadratic family (all three layouts):
            benchmark_build_reserved!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);          // unaligned indirect
            benchmark_build_reserved!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);  // aligned indirect
            benchmark_build_reserved!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);    // direct
            // Cuckoo family (aligned + unaligned indirect, + direct):
            benchmark_build_reserved!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_build_reserved!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_build_reserved!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
            // Linear (for reference):
            benchmark_build_reserved!(linear_probing_table::HashTable::<u64>, u64)(n, capacity);
            benchmark_build_reserved!(direct_simd_linear_probing::HashTable::<u64>, u64)(n, capacity);
            benchmark_build_reserved!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);
          } // BENCH_BUILD
        }
    }
}

