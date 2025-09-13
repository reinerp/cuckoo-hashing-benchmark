#![allow(unused)]
#![allow(unsafe_op_in_unsafe_fn)]

use std::{hint::black_box, time::Instant};

mod control;
mod quadratic_probing_table;
mod u64_fold_hash_fast;
mod uunwrap;

macro_rules! benchmark_find {
    ($table:ty, $v:ty) => {
        (|n: usize| {
            let mut table = <$table>::with_capacity(n);
            let mut rng = fastrand::Rng::with_seed(123);
            for _ in 0..n {
                let key = rng.u64(..);
                table.insert(key, <$v>::default());
            }
            let start = Instant::now();
            const ITERS: usize = 1000_000_000;
            let mut found = 0;
            for _ in 0..ITERS {
                let key = rng.u64(..);
                found += table.get(&key).is_some() as usize;
            }
            black_box(found);
            let duration = start.elapsed();
            println!("{}/{n}: {:.2} ns/op", stringify!($table), duration.as_nanos() as f64 / ITERS as f64);
        })
    }
}

fn main() {
    benchmark_find!(quadratic_probing_table::HashTable::<u64>, u64)(1_000_000);
    benchmark_find!(hashbrown::HashMap::<u64, u64>, u64)(1_000_000);
}
