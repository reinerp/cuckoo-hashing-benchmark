// Standalone test for rebucketing functionality
#![allow(dead_code)]

mod control;
mod dropper;
mod u64_fold_hash_fast;
mod uunwrap;
mod aligned_cuckoo_table;

use aligned_cuckoo_table::HashTable;
use std::collections::HashMap;

fn main() {
    println!("Testing rebucketing functionality...\n");

    // Test 1: Basic rebucketing
    println!("Test 1: Basic rebucketing from small table");
    let mut table = HashTable::new();
    for i in 1..=100 {
        table.insert(i, i * 2);
    }

    // Verify all elements
    let mut success = true;
    for i in 1..=100 {
        if table.get(&i) != Some(&(i * 2)) {
            println!("  FAILED: Key {} not found or has wrong value", i);
            success = false;
        }
    }
    if success {
        println!("  PASSED: All 100 elements inserted and retrieved correctly");
    }
    println!("  Final table size: {} buckets, {} items", table.num_buckets(), table.len());

    // Test 2: Multiple rebucketings
    println!("\nTest 2: Multiple successive rebucketings");
    let mut table = HashTable::new();
    let mut reference = HashMap::new();
    let mut rng = fastrand::Rng::with_seed(12345);

    for _ in 0..500 {
        let key = rng.u64(1..10000);
        let value = rng.u64(..);
        table.insert(key, value);
        reference.insert(key, value);
    }

    success = true;
    for (&key, &value) in &reference {
        if table.get(&key) != Some(&value) {
            println!("  FAILED: Key {} has wrong value", key);
            success = false;
            break;
        }
    }

    if success {
        println!("  PASSED: All {} elements correct after multiple rebucketings", reference.len());
    }
    println!("  Final table size: {} buckets, {} items", table.num_buckets(), table.len());

    // Test 3: Updates during growth
    println!("\nTest 3: Updates during table growth");
    let mut table = HashTable::new();

    // Initial insertions
    for i in 1..=50 {
        table.insert(i, i * 10);
    }

    // Update some values
    for i in 1..=25 {
        table.insert(i, i * 20);
    }

    // More insertions to trigger rebucketing
    for i in 51..=200 {
        table.insert(i, i * 10);
    }

    success = true;
    for i in 1..=25 {
        if table.get(&i) != Some(&(i * 20)) {
            println!("  FAILED: Updated key {} has wrong value", i);
            success = false;
            break;
        }
    }
    for i in 26..=200 {
        if table.get(&i) != Some(&(i * 10)) {
            println!("  FAILED: Key {} has wrong value", i);
            success = false;
            break;
        }
    }

    if success {
        println!("  PASSED: All 200 elements correct with updates preserved");
    }
    println!("  Final table size: {} buckets, {} items", table.num_buckets(), table.len());

    println!("\nAll rebucketing tests completed!");
}
