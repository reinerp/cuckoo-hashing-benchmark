#![allow(unused)]
#![allow(unsafe_op_in_unsafe_fn)]
#![feature(likely_unlikely)]
use std::{hint::black_box, time::Instant};

mod control;
mod quadratic_probing_table;
mod aligned_quadratic_probing_table;
mod aligned_cuckoo_table;
mod balancing_cuckoo_table;
mod unaligned_cuckoo_table;
mod u64_fold_hash_fast;
mod uunwrap;
mod scalar_cache_line_aligned_table;
mod scalar_unaligned_table;
mod scalar_cuckoo_table;

const ITERS: usize = 100_000_000;

trait HashTableExt {
    fn print_stats(&self) {}
}

impl HashTableExt for hashbrown::HashMap<u64, u64> {}

macro_rules! benchmark_find_miss {
    ($table:ty, $v:ty) => {
        (|n: usize| {
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
            println!("find_miss {}/{n}: {:.2} ns/op", stringify!($table), duration.as_nanos() as f64 / ITERS as f64);
            table.print_stats();
        })
    }
}

macro_rules! benchmark_find_hit {
    ($table:ty, $v:ty) => {
        (|n: usize| {
            let mut table = <$table>::with_capacity(n);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let outer_iters = ITERS / n;
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
            println!("find_hit  {}/{n}: {:.2} ns/op", stringify!($table), duration.as_nanos() as f64 / true_iters as f64);
        })
    }
}

macro_rules! benchmark_find_latency {
    ($table:ty, $v:ty) => {
        (|n: usize| {
            let mut table = <$table>::with_capacity(n);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let outer_iters = ITERS / n;
            let true_iters = outer_iters * n;
            let start = Instant::now();
            let mut found = 0;
            for _ in 0..outer_iters {
                let mut rng = fastrand::Rng::with_seed(123);
                let mut prev_value = 0;
                for _ in 0..n {
                    let key = rng.u64(..) ^ prev_value;
                    prev_value = *table.get(&key).unwrap();
                }
                black_box(prev_value);
            }
            let duration = start.elapsed();
            println!("find_hit_latency  {}/{n}: {:.2} ns/op", stringify!($table), duration.as_nanos() as f64 / true_iters as f64);
        })
    }
}

fn main() {
    let mi = 1 << 20;
    for load_factor in [4, 5, 6, 7] {
        println!("load factor: {}/8", load_factor);
        let n = mi * load_factor / 8;
        benchmark_find_miss!(quadratic_probing_table::HashTable::<u64>, u64)(n);
        benchmark_find_miss!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n);
        if load_factor < 7 {
            benchmark_find_miss!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n);
        }
        benchmark_find_miss!(aligned_cuckoo_table::HashTable::<u64>, u64)(n);
        benchmark_find_miss!(balancing_cuckoo_table::HashTable::<u64>, u64)(n);
        benchmark_find_miss!(scalar_cache_line_aligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_miss!(scalar_unaligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_miss!(scalar_cuckoo_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_miss!(hashbrown::HashMap::<u64, u64>, u64)(n);

        benchmark_find_hit!(quadratic_probing_table::HashTable::<u64>, u64)(n);
        benchmark_find_hit!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n);
        benchmark_find_hit!(aligned_cuckoo_table::HashTable::<u64>, u64)(n);
        benchmark_find_hit!(balancing_cuckoo_table::HashTable::<u64>, u64)(n);
        if load_factor < 7 {
            benchmark_find_hit!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n);
        }
        benchmark_find_hit!(scalar_cache_line_aligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_hit!(scalar_unaligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_hit!(scalar_cuckoo_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_hit!(hashbrown::HashMap::<u64, u64>, u64)(n);

        benchmark_find_latency!(quadratic_probing_table::HashTable::<u64>, u64)(n);
        benchmark_find_latency!(aligned_quadratic_probing_table::HashTable::<u64>, u64)(n);
        benchmark_find_latency!(aligned_cuckoo_table::HashTable::<u64>, u64)(n);
        benchmark_find_latency!(balancing_cuckoo_table::HashTable::<u64>, u64)(n);
        if load_factor < 7 {
            benchmark_find_latency!(unaligned_cuckoo_table::HashTable::<u64>, u64)(n);
        }
        benchmark_find_latency!(scalar_cache_line_aligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_latency!(scalar_unaligned_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_latency!(scalar_cuckoo_table::U64HashSet::<u64>, u64)(n);
        benchmark_find_latency!(hashbrown::HashMap::<u64, u64>, u64)(n);


    }
}
