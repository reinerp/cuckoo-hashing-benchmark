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

const ITERS: usize = 10_000_000;
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

fn drop_spaces(s: &str) -> String {
    s.split_whitespace().collect()
}

macro_rules! benchmark_find_miss {
    ($table:ty, $v:ty) => {
        (|n: usize| {
            print!("find_miss  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(n);
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
        (|n: usize| {
            print!("find_hit  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(n);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let outer_iters = ITERS.div_ceil(n);
            let true_iters = outer_iters * n;
            let start = Instant::now();
            let mut found = 0;
            for _ in 0..outer_iters {
                let mut rng = fastrand::Rng::with_seed(123);
                for _ in 0..n {
                    let key = rng.u64(..);
                    found += table.get(&key).is_some() as usize;
                }
            }
            black_box(found);
            let duration = start.elapsed();
            println!(
                "{:.2} ns/op",
                duration.as_nanos() as f64 / true_iters as f64
            );
        })
    };
}

macro_rules! benchmark_find_latency {
    ($table:ty, $v:ty) => {
        (|n: usize| {
            print!(
                "find_hit_latency  {}/{n}: ",
                drop_spaces(stringify!($table))
            );
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(n);
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
        (|n: usize| {
            print!("insert_erase  {}/{n}: ", drop_spaces(stringify!($table)));
            std::io::stdout().flush().unwrap();
            let mut table = <$table>::with_capacity(n);
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

fn main() {
    for lg_mi in [10, 15, 20, 25] {
        println!("mi: 2^{lg_mi}");
        let mi = 1 << lg_mi;
        for load_factor in [4, 5, 6, 7] {
            println!("load factor: {}/8", load_factor);
            let n = mi * load_factor / 8;
            macro_rules! benchmark_all {
                ($benchmark:ident) => {
                    // Our cuckoo tables fail on repeated insert_erase on high load factors. We need to extend
                    // them with BFS and rehashing support. Until then, we skip the benchmarks.
                    let is_insert_and_erase = std::stringify!($benchmark) == "benchmark_insert_and_erase";
                    // $benchmark!(aligned_double_hashing_table::HashTable::<u64>, u64)(n);
                    $benchmark!(quadratic_probing_table::HashTable::<u64>, u64)(n);
                    // $benchmark!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n);
                    // if load_factor < 7 && (!is_insert_and_erase || load_factor < 6) && lg_mi < 25 {
                    //     // This cuckoo table doesn't work for large load factors.
                    //     $benchmark!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n);
                    // }
                    $benchmark!(aligned_cuckoo_table::HashTable::<u64>, u64)(n);
                    $benchmark!(direct_simd_cuckoo_table::HashTable::<u64>, u64)(n);
                    // if !is_insert_and_erase || load_factor < 7 {
                    //     $benchmark!(balancing_cuckoo_table::HashTable::<u64>, u64)(n);
                    // }
                    // $benchmark!(scalar_cache_line_aligned_table::U64HashSet::<u64>, u64)(n);
                    // $benchmark!(scalar_unaligned_table::U64HashSet::<u64>, u64)(n);
                    if !is_insert_and_erase || load_factor < 6 {
                        $benchmark!(scalar_cuckoo_table::U64HashSet::<u64>, u64)(n);
                    }
                    $benchmark!(hashbrown::HashMap::<u64, u64>, u64)(n);
                }
            }

            benchmark_all!(benchmark_find_miss);
            benchmark_all!(benchmark_find_hit);
            benchmark_all!(benchmark_find_latency);
            benchmark_all!(benchmark_insert_and_erase);
        }
    }
}
