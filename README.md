# cuckoo-hashing-benchmark
Benchmark for cuckoo hashing

## Findings so far

**Cuckoo hashing** works really well, both in SIMD and non-SIMD context. Improves find_miss, find_hit, and insertion.
* Big improvement on find_miss and insertion, especially at large load factors, and especially for in-TLB (nearly-in-cache) tables.
* A nice bonus is that we don't need to check for empty slots; checking for key matches is sufficient. This speeds up find_hit. When in-cache, in fact it can be 100% branchless in the Direct SIMD case, which helps a lot.
* TODO: BFS loop should be "hash then search", not "search then hash". That avoids redundant hashing operations.
* For huge tables (out-of-cache, out-of-TLB), insertion is a little slower.
* Cuckoo actually has *faster* insertion than non-cuckoo, with advantage growing as load factor increases. This relies on being able to compute the "alternative" cuckoo bucket from just the tag array, as is possible with the `base_hash ^ hash(tag)` approach.
* Unaligned cuckoo hashing seems beneficial

**Indirect SIMD** probing works really well at large load factors, where it improves almost everything. 

**Direct SIMD** probing is an improvement on find_hit, but worse on find_miss and insertion.
* The main issue is that it can't probe as fast: indirect SIMD probing (on 1 byte tags) can probe 8-16 values per SIMD instruction, whereas direct SIMD probing on u64 probes 2 values per SIMD instruction. 
* Also for out-of-cache tables and find_miss/insertion it actually makes *more* cache misses because it fetches 8 bytes per key rather than 1 byte per key.

**Localized SIMD** probing (probe a 1-byte-tag array, but it's local to the cache line, like Folly F14) is somewhere between Direct SIMD and Indirect SIMD in performance. Better than Direct SIMD at find_miss (because we scan 7 values per cache line, not 4); better than Indirect SIMD at find_hit (because keys are on the same cache line as tags). Seems to have some instruction overhead compared to Indirect SIMD, presumably from slightly longer instruction sequences for bucket indexing.

**Scalar** probing is mostly an improvement on find_hit_latency, and is mostly worse on everything else. At extremely low load factors (12.5%-25%) it can sometimes be the fastest at insertion, but not reliably so.
* Probes are relatively slower.
* At tables >=2^15 and load factors >4/8 we see big penalties from branch misprediction on find_hit. std::hint::select_unpredictable improves performance considerably.
* The main advantage over Indirect SIMD, namely only one cache line per lookup, is achieved better by Direct SIMD. This applies to out-of-cache latency as well.

**Cuckoo early return** is favored at tiny load factors (12.5%-50%), but becomes very unfavorable at larger load factors. I believe is because of the implied branch mispredicts.

**Very low load factor tables** see different effects than most of the above. Both Cuckoo hashing and SIMD probing shine most at non-tiny load factors (>30%), because they are ways to make long probe sequences more efficient. But if load factors are very small (10-50%) then probe length tends to matter less and we care more about simple instruction count. Eventually scalar probing becomes viable, because it has lower latency and instruction count: no moving data between general purpose registers and SIMD registers. That said, this comes at a huge cost in memory footprint.

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
* Do you extremely prioritize time over memory footprint? (Do you tolerate ~7x memory footprint for ~1.25x speedup?)
  * Then use scalar probing with 12.5% load factor.
* Is the table entirely in cache (up to ~1000 entries, table accessed in a high-iteration-count loop)
  * Then use unaligned cuckoo hashing with Indirect SIMD
* Are most operations "lookup hits", i.e. on keys that are already in the table?
  * Does it have integer-like keys (fixed-size 4-8 byte keys)?
    * Then use (aligned) cuckoo hashing with Direct SIMD.
  * Does it have string-like keys (variable-length payloads, behind a pointer)?
    * The using (aligned) cuckoo hashing with Localized SIMD.
* Otherwise, use unaligned cuckoo hashing with Indirect SIMD.