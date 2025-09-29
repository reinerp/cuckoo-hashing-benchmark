#![allow(unused)]
#![allow(unsafe_op_in_unsafe_fn)]
#![feature(likely_unlikely)]
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

const ITERS: usize = 100_000_000;
const TRACK_PROBE_LENGTH: bool = false;

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
    // {
    //     let mut rng = fastrand::Rng::with_seed(123);
    //     let len = 1usize << 28;
    //     let mut v = (0..len).map(|i| rng.u8(..)).collect::<Vec<_>>();
    //     const CACHE_LINE_SIZE: usize = 128;
    //     let mut previous_nanos = 0.0;
    //     for n_cache_lines in 1..=4 {
    //         print!("random reads of {} cache lines: ", n_cache_lines);
    //         std::io::stdout().flush().unwrap();
    //         let start = Instant::now();
    //         let mut xor = 0;
    //         for i in 0..ITERS {
    //             let first_line = ((rng.usize(..) % len) / CACHE_LINE_SIZE) * CACHE_LINE_SIZE;
    //             for j in 0..n_cache_lines {
    //                 xor ^= v[(first_line + j * CACHE_LINE_SIZE) % len];
    //             }
    //         }
    //         black_box(xor);
    //         let duration = start.elapsed();
    //         let nanos = duration.as_nanos() as f64 / ITERS as f64;
    //         let delta = nanos - previous_nanos;
    //         println!("{:.2} ns/op ({:.2} ns/op incremental)", nanos, delta);
    //         previous_nanos = nanos;
    //     }
    // }

    for lg_mi in [25] {  // Focus on 2^15 for fast testing
        println!("mi: 2^{lg_mi}");
        let mi = 1 << lg_mi;
        for load_factor in [16, 24, 28] {  // Use a single moderate load factor
            println!("load factor: {:.1}%", load_factor as f64 / 32.0 * 100.0);
            let n = mi * load_factor / 32;
            let capacity = mi * 7 / 8;
            macro_rules! benchmark_all {
                ($benchmark:ident) => {
                    // Our cuckoo tables fail on repeated insert_erase on high load factors. We need to extend
                    // them with BFS and rehashing support. Until then, we skip the benchmarks.
                    let is_insert_and_erase = std::stringify!($benchmark) == "benchmark_insert_and_erase";
                    // $benchmark!(aligned_double_hashing_table::HashTable::<u64>, u64)(n, capacity);
                    $benchmark!(quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
                    // $benchmark!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n, capacity);
                    $benchmark!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                    $benchmark!(aligned_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                    // $benchmark!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                    // $benchmark!(direct_simd_quadratic_probing::HashTable::<u64>, u64)(n, capacity);
                    // if !is_insert_and_erase || load_factor < 7 {
                    //     $benchmark!(balancing_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                    // }
                    // {
                    //     let n = n * 7 / 8;
                    //     $benchmark!(localized_simd_cuckoo_table::HashTable::<u64>, u64)(n, capacity);
                    // }
                    // $benchmark!(scalar_cache_line_aligned_table::U64HashSet::<u64>, u64)(n, capacity);
                    // $benchmark!(scalar_unaligned_table::U64HashSet::<u64>, u64)(n, capacity);
                    // if !is_insert_and_erase || load_factor < 6 {
                    //     $benchmark!(scalar_cuckoo_table::U64HashSet::<u64>, u64)(n, capacity);
                    // }
                    // $benchmark!(hashbrown::HashMap::<u64, u64>, u64)(n, capacity);
                }
            }

            // Disable other benchmarks for now, focus on probe histogram
            // benchmark_all!(benchmark_find_miss);
            // benchmark_all!(benchmark_find_hit);
            // benchmark_all!(benchmark_find_latency);
            // benchmark_all!(benchmark_insert_and_erase);

            // Run the probe histogram benchmarks
            benchmark_all!(benchmark_probe_histogram);
            println!();
            benchmark_all!(benchmark_insertion_probe_histogram);
        }
    }
}

