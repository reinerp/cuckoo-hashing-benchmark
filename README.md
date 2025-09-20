# cuckoo-hashing-benchmark
Benchmark for cuckoo hashing

## Findings so far

**Cuckoo hashing** works really, both in SIMD and non-SIMD context. Improves find_miss and insertion, neutral on find_hit.
* Big improvement on find_miss and insertion, especially at large load factors, and especially for in-TLB (nearly-in-cache) tables.
* TODO: BFS loop should be "hash then search", not "search then hash". That avoids redundant hashing operations.
* For huge tables (out-of-cache, out-of-TLB), insertion is a little slower.
* Cuckoo actually has *faster* insertion than non-cuckoo for in-cache tables, with advantage growing as load factor increases. However, for out-of-cache tables (2^25) cuckoo falls behind. I suspect this is because quadratic probing stays on the same line (or few lines) for a while, whereas cuckoo doesn't. Specifically, with 8-byte probe sequences, the probe ranges are at the following offsets from the starting position: 0 bytes, 8 bytes, 24 bytes, 48 bytes, 80 bytes. The first 3 will usually be on the same cache line, and the first 5 will usually be on two cache lines. This is 3 times better than cuckoo probing, which takes one cache line per probe.

**Indirect SIMD** probing works really well at large load factors, where it improves almost everything. 

**Direct SIMD** probing is an improvement on find_hit, but worse on find_miss and insertion.
* The main issue is that it can't probe as fast: indirect SIMD probing (on 1 byte tags) can probe 8-16 values per SIMD instruction, whereas direct SIMD probing on u64 probes 2 values per SIMD instruction. 
* Also for out-of-cache tables and find_miss/insertion it actually makes *more* cache misses because it fetches 8 bytes per key rather than 1 byte per key.

**Scalar** probing is mostly an improvement on find_hit_latency, and is mostly worse on everything else.
* Probes are relatively slower.
* At tables >=2^15 and load factors >4/8 we see big penalties from branch misprediction on find_hit. std::hint::select_unpredictable improves performance considerably.
* The main advantage over Indirect SIMD, namely only one cache line per lookup, is achieved better by Direct SIMD. This applies to out-of-cache latency as well.

## Overall recommendations

u64 (and probably also u32) keys:
* Are the majority of operations unseen keys (either for lookup or insert)?
  * If yes, then is it a *large*, *build-mostly* table? (More precisely: >2^25 elements, and majority of operations are insertions?)
    * If yes, then *don't* use cuckoo hashing. Use indirect SIMD with quadratic probing, i.e. what hashbrown / Swiss Tables do.
    * If no, then use cuckoo hashing with indirect SIMD.
  * If no, then use cuckoo hashing with direct SIMD

string keys (and other large keys):
* Direct SIMD isn't applicable. Then it's just a choice between cuckoo and quadratic probing.
* Is it a large, build-mostly table?
  * If yes, do indirect SIMD with quadratic probing, i.e. what hashbrown / Swiss Tables do.
  * If no, do indirect SIMD with cuckoo hashing.

Overall flowchart:
* Do you need fast union/intersect operations between multiple tables?
  * Then use Robin Hood hashing, to support traversals ordered by hash.
* Is it a *large*, *build-mostly* table? (More precisely: >2^25 elements, and majority of operations are insertions?)
  * Then use indirect SIMD with quadratic probing, i.e. what hashbrown / Swiss Tables do.
  * TODO: what about cuckoo hashing with longer probes, e.g. repeated 2x?
* Does it have integer-like keys (fixed-size 4-8 byte keys), and are most operations on already-seen keys?
  * Then use cuckoo hashing with Direct SIMD.
* Otherwise, use cuckoo hashing with Indirect SIMD.