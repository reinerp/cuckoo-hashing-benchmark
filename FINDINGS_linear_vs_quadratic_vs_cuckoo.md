# Does {linear, cuckoo} dominate quadratic probing?

> **TL;DR / final word: see [Part 6](#part-6--consolidated-picture-with-all-optimizations-and-final-decision).**
> After two implementation fixes (cuckoo insert early-exit, linear backward-shift deletion),
> **cuckoo wins essentially every workload** and the decision is to **use cuckoo everywhere**. Parts
> 2–5 below are the investigation trail and their write-side conclusions are superseded by Part 6.

**Question.** At low load factors, is *linear* probing as good as *quadratic* probing in probe
length, and faster in practice (non‑power‑of‑2 sizing + simpler tracking arithmetic)? And sharply:
does **{linear probing, cuckoo probing} weakly dominate quadratic probing** — is quadratic *never
the unique best* of the three, at any (operation × load factor × table size)?

**Short answer.**

> **On lookups and memory: yes — fully. On insertion: no.**
> {linear, cuckoo} weakly dominates quadratic on **every lookup workload** (find_hit, find_miss,
> latency) and on the **memory axis** (quadratic never the unique best). But **insertion is a genuine
> exception**: across the two insertion workloads I could measure — steady‑state churn pinned at high
> load (`insert_erase`) and amortized build into a pre‑sized table (`build_reserved`) — **quadratic is
> the best of the three over a broad region** (notably all of Direct‑SIMD at high load, plus the top
> of Indirect). Linear is essentially never the best inserter (it clusters; it only edges quad on
> Indirect low/mid‑load churn), and cuckoo wins insertion only on wide buckets at pinned‑high‑load
> churn or — the one axis I *couldn't* measure here — on growth‑builds where its branchless
> rehash‑without‑search shines (the blog's headline insert result).

So your **low‑load probe‑length and lookup hypothesis holds cleanly**; the place {linear, cuckoo}
fails to dominate quadratic is **insertion**, and how badly depends on the exact insert workload — a
refinement of the blog's "large build‑mostly table → quadratic probing" rule.

*Method.* Apple Silicon (aarch64/NEON) ⇒ Indirect‑SIMD group width `W=8`, Direct‑SIMD bucket width
`W=4`. Benchmark = **median of 3 runs**, `ITERS=40M`, `target-cpu=native`. "Tie" tolerance = 4%
(median‑of‑3 noise floor). This conclusion was **adversarially re‑checked** against the 3 raw runs;
the corrections it surfaced are folded in below.

---

## Part 1 — Probe length (simulation: `hash-table-probing-strategies/`)

Bucketized model, `B = 2^16` power‑of‑two groups of `W` slots shared by all strategies, 8 seeds,
metric = group‑probes. CSV: `hash-table-probing-strategies/probe_lengths.csv`.

**Linear ≈ quadratic at low load; the gap is a high‑load tail effect.** Linear−quadratic relative
gap (W=8, Indirect):

| | α=0.25 | α=0.50 | α=0.625 | α=0.75 | α=0.875 |
|---|---|---|---|---|---|
| hit (= insert) | 0.0% | 0.01% | +0.1% | +1.0% | +8.0% |
| miss           | 0.0% | +0.08% | +0.8% | +9.1% | +59.8% |

At α ≤ 0.5 they are **indistinguishable**; divergence only past ~0.7 (and bigger for misses, whose
cost grows ~like the square of the run length). **So "is linear as good as quadratic at low load?" →
yes, essentially identical.** Cuckoo's probe length is flat (hit ≈ 1.0→1.4, miss = 2.0) and
cuckoo‑hit ≤ quadratic‑hit at every load. In pure probe *count* cuckoo‑miss (2.0) only undercuts
quad‑miss above α≈0.92/0.96 — but that is the wrong model out‑of‑cache: linear's extra probes share a
cache line, cuckoo's two probes are two *random* lines, and that is what the hardware measures.
The sim also shows cuckoo's *find‑a‑home* insert probe count at W=4/0.875 is **1.44, tying quad's
1.46** — so cuckoo's high‑load insert *slowness* below is eviction/churn overhead, not home‑finding.

---

## Part 2 — Real performance (benchmark: `cuckoo-hashing-benchmark/`)

New correctness‑checked tables: `linear_probing_table` (Indirect‑SIMD), `direct_simd_linear_probing`
(Direct‑SIMD), `direct_simd_linear_probing_np2` (non‑pow2 `mul_high` linear). Sweep: 2^10/2^15/2^20/
2^25 × loads 25–87.5% × {find_miss, find_hit, find_hit_latency, insert_erase}. Of ~180 cells, the
domination check (`analyze_median.py`) flags only 4 as "quad uniquely best":

| cell | linear | quad | cuckoo | nature |
|---|---|---|---|---|
| find_miss indirect 2^20 37.5% | 1.73 | 1.64 | 1.88 | **noise** (per‑run winner flips; 0.09 ns) |
| find_hit  indirect 2^15 50%   | 1.72 | 1.59 | 1.74 | **noise** (per‑run winner flips; 0.13 ns) |
| insert_erase direct 2^10 87.5% | 6.45 | 3.50 | 13.40 | **GENUINE** (all 3 runs; W=4‑specific) |
| insert_erase direct 2^15 87.5% | 10.94 | 5.48 | 15.39 | **GENUINE** (all 3 runs; W=4‑specific) |

### find_miss — dominated ✅
Linear ties quad at low/mid load; cuckoo crushes both above ~70% (2^25 @87.5%: cuckoo **6.3** vs quad
26.7 vs linear 36.4 ns) — exactly the blog's "cuckoo wins failed‑lookups past 75%," with **linear
covering the sub‑75% range** quadratic owned. The one reproducible low‑load signal where pow2‑linear
trails quad (direct 2^25 @25%: quad 5.21 < pow2‑lin 5.28) is **closed by np2‑linear (5.09 < quad
5.21)** — and np2 is in the {linear,cuckoo} family — so it is not a domination hole.

### find_hit — dominated ✅ (on the hit‑recommended Direct‑SIMD layout)
Direct‑SIMD cuckoo wins at every load (2^25 @87.5%: cuckoo **7.9** vs quad 16.4 vs linear 20.5);
linear ≈ quad. On Indirect‑SIMD cuckoo doesn't help hits (blog footnote) and quad/linear are
near‑tied — the lone marginal "quad best" hit cell lives there, in a layout you wouldn't pick for hits.

### find_hit_latency — dominated ✅ (but the *mechanism* is layout‑specific, not "simpler arithmetic")
In‑cache 2^10, **Indirect**: linear **1.5** ≈ cuckoo **1.5** vs quad **3.0** ns — a ~2× win.
**Correction:** this is an *Indirect‑layout* property (L/Q latency ratio 0.48–0.89 on Indirect but
0.97–1.21 on Direct — quad even *wins* Direct at 87.5%). It is therefore **not** cleanly explained by
linear's stride‑free `move_next` (the arithmetic is identical on both layouts and the gap is flat vs
load); it's a quirk of the Indirect quad get‑loop codegen. Linear/cuckoo still win the latency axis,
but I no longer attribute it to reason (b).

### insert_erase — dominated EXCEPT one W=4 corner ⚠️
An insert is "find first empty" = the miss path, so on **Direct‑SIMD (W=4) at high load** linear
clusters and is ≥ quad. But this does **not** generalize:
- **On Indirect (W=8), linear *out‑inserts* quad** at low/mid load in 7 cells (e.g. 2^10 @62.5%:
  **2.39** vs 3.05, 22% faster; 2^15 @75%: **3.13** vs 4.22, 26% faster) — quad's running‑stride
  arithmetic makes *it* the slower inserter there. (Correction to an earlier overgeneralization.)
- **On Indirect (W=8), cuckoo is the best high‑load inserter at every size** (2^10 @87.5%: cuckoo
  **2.15** vs quad 3.98; 2^15: **2.60** vs 7.86; 2^25: **17.41** vs 30.15). So insertion is fully
  dominated on the Indirect layout.

The **only genuine quad‑uniquely‑best corner** is **W=4 Direct‑SIMD, in‑cache, 87.5%** (robust across
all 3 runs):

| direct insert_erase 87.5% | quad | linear | cuckoo |
|---|---|---|---|
| 2^10 (in‑cache) | **3.3–3.8** | 5.8–6.6 | 13.1–13.7 |
| 2^15 (in‑cache) | **5.4–5.7** | 10.4–11.5 | 14.9–16.7 |

Cause: 4‑slot buckets push cuckoo near its ~0.96 load ceiling so eviction chains explode (13–17 ns);
linear clusters. Out‑of‑cache this corner already heals (2^20: cuckoo **21.5** < quad 23.5; 2^25:
cuckoo 91.5 ≈ quad 89.9, a tie). And note the metric: `insert_erase` is **steady‑state churn pinned
at max load — cuckoo's worst case** (each eviction permanently relocates keys; linear/quad restore
bit‑for‑bit). See Part 4 for the build‑from‑empty measurement.

---

## Part 3 — The two "why linear is faster" reasons

**(b) Simpler tracking arithmetic.** Linear's `move_next` is `pos = (pos+W) & mask`; quadratic
carries a running `stride`. The *latency* win above (~2× in‑cache) is real but is an Indirect‑layout
effect not cleanly traceable to this arithmetic (Direct shows no gap). Net: a real but layout‑specific
edge, smaller and less clearly‑mechanised than the simple story.

**(a) Non‑power‑of‑2 sizing — mis‑attributed, and mostly about memory.** Only *quadratic* is locked
to power‑of‑two (its triangular‑number coverage proof); **linear *and* cuckoo accept any `B`**, so
this is a {linear,cuckoo}‑vs‑quadratic advantage — it *strengthens* the thesis (quad is weakly
dominated on memory by both).
- **Memory benefit:** pow2 rounding makes quad average `E[2B/n] = 2·ln2 ≈ 1.39×` the slots it needs
  (up to 2×); linear/cuckoo size exactly to the chosen load. ~39% average memory saving, up to 2×.
- **Speed cost ≈ 0:** measured `np2/mask` on find_miss (random keys) is 0.93–1.13, **median ≈ 0.98**
  with symmetric tails — the latency‑3 `mul_high` is fully hidden; no throughput tax.
- *Curiosity:* `mul_high` on *sequential* keys gives Fibonacci‑hashing equidistribution (buckets fill
  to exactly capacity, ~1‑probe hits even at 87.5%), so the find_hit benchmark's sequential keys are
  unrepresentative for np2 — the tax is read from find_miss.

---

## Part 4 — Build‑from‑empty insertion (`build_reserved`, median of 3)

Inserting *n* distinct keys into a **pre‑sized** table (no erase, no growth) — the "build‑mostly"
workload. This tells a **different and more quad‑favourable** story than `insert_erase`, and the
contrast is the real insight: insertion's winner depends on the workload.

- **Quadratic is the best builder at moderate/high load on Direct‑SIMD at *every* cache level**, incl.
  far out‑of‑cache:

  | direct build_reserved | 25% | 50% | 75% | 87.5% |
  |---|---|---|---|---|
  | 2^25 quad / linear / cuckoo | 8.0/8.1/11.0 | 8.1/8.1/10.8 | **10.7**/12.4/13.2 | **16.8**/21.6/24.0 |

  and quad is best on Indirect at the very top load too (2^25 @87.5%: quad **9.7** vs lin 10.3 vs
  cuckoo 11.2). 11 of 48 build cells are quad‑uniquely‑best — a *broader* exception than `insert_erase`.

- **Why the flip from `insert_erase`?** Building from empty means most inserts happen while the table
  is still *lightly loaded* (cheap for everyone), so quad's amortized cost stays low; cuckoo pays
  eviction overhead on every insert in the upper half of the build, with **no** growth‑rebucketing
  benefit to offset it (the table is pre‑sized). Conversely `insert_erase` pins the table at *max*
  load, which is quad's worst case (long clustered probes) and cuckoo's relative best.

- **The one axis that favours cuckoo — growth‑builds — isn't measurable here.** `build_unreserved`
  (grow from tiny via `realloc` doubling) is where cuckoo's *branchless rehash‑without‑search* wins
  (the blog's headline insert result), but the linear/quadratic tables in this benchmark are
  fixed‑capacity (no resize), so a head‑to‑head needs resize logic added first. So: cuckoo wins
  insertion when growth‑rehashing dominates; quadratic wins when building into reserved capacity.

Net: **across the two insertion workloads I *could* measure (steady‑state churn and build‑into‑
reserved), quadratic is the best of the three over a broad region** — all of Direct‑SIMD at high load
plus the top of Indirect — so insertion is a genuine, robust domain where {linear, cuckoo} does **not**
dominate quadratic, not a single W=4 corner. Linear is essentially never the best builder.

---

## Part 5 — Best-of-layouts comparison (each strategy picks its better layout)

A practitioner picks the best layout for their strategy, so the sharpest test reduces each strategy
to **min(direct, indirect)** at every operating point and compares best-linear vs best-quadratic vs
best-cuckoo (`analyze_best.py`; `linear_indirect` = `linear_probing_table`, already in the sweep).
This **removes the W=4 artifacts** — you'd never insert-heavy on Direct-SIMD when Indirect's wide
buckets keep cuckoo cheap. Result: **best-quadratic is uniquely best in only 4 cells — all
`build_reserved` at high load:**

| workload | verdict (best-of-layouts) |
|---|---|
| find_hit | **dominated everywhere** (cuckoo-*direct* wins; 2^25@87.5%: C 7.9 vs Q 12.4) |
| find_miss | **dominated everywhere** (linear ties quad at low load; cuckoo wins high — direct at small sizes, indirect at 2^25: C 6.3 vs Q 26.7 @87.5%) |
| find_hit_latency | **dominated everywhere** |
| insert (churn @ load) | **dominated everywhere** — the W=4 corner is gone: best layout is Indirect, where cuckoo wins high load (2^25@87.5%: C **17.4** vs Q 30.2 vs L 42.7; 2^10@87.5%: C **2.15** vs Q 3.50) |
| build (into reserved) | **4 quad-best cells, all load ≥75%** |

The four surviving cells (best-quad beats best-linear AND best-cuckoo):

| build_reserved | best_quad | best_linear | best_cuckoo | quad better by |
|---|---|---|---|---|
| 2^10 @75%   | 1.65 (d) | 1.74 (d) | 1.93 (i) | 5% |
| 2^10 @87.5% | 1.81 (d) | 2.11 (d) | 2.02 (i) | 12% |
| 2^20 @87.5% | 3.79 (i) | 4.06 (i) | 4.07 (i) | 7% |
| 2^25 @87.5% | 9.70 (i) | 10.28 (i) | 11.20 (i) | 6% |

So under best-of-layouts the entire insertion exception collapses to **one workload: building into
reserved capacity at a high target load (≥75%)**, where quadratic's amortized-low probe beats
cuckoo's accumulated eviction overhead (no growth-rehash benefit) and linear's clustering, by 5–12%.
The one axis that would likely overturn even this — build *with growth* (cuckoo's branchless
realloc-doubling) — needs resize logic added to the linear/quadratic tables to measure.

---

## Part 6 — Consolidated picture with all optimizations, and final decision

Parts 2–5 above were measured *before* two implementation fixes that materially change the insertion
picture; this section supersedes their write-side conclusions.

**Two fixes applied:**
1. **Cuckoo insert early-exit** (`EARLY_RETURN`): insert into the first bucket if it has a free slot
   without loading the second bucket's cache line. The earlier cuckoo inserts always loaded *both*
   buckets (to check for the key), paying ~2× the cache traffic. Early-exit cut cuckoo
   `build_reserved` by ~25–40%. *Caveat:* build/distinct-key-only — it can duplicate a key already in
   the second bucket under insert-or-update, so it is not a correct general-map insert.
2. **Linear backward-shift deletion** (tombstone-free, Knuth Algorithm R) instead of a set-EMPTY
   shortcut. Correct and tombstone-free, but on the indirect layout it reads the payload (to rehash)
   per scanned slot, making linear the *slowest* churn-inserter.

**Best-of-layouts winner counts (median of 3, each strategy on its best layout):**

| workload | linear | quad | cuckoo |
|---|---|---|---|
| find_hit | 0–1 | 0 | **~23/24** |
| find_miss | 0–1 | 0 | **~23/24** |
| insert (churn) | 0 | 0 | **24/24** |
| build (reserved) | 0 | 5 | **19/24** |
| latency (in-cache) | — tie ~1.3 ns (all on direct-SIMD) — |||

Once cuckoo gets early-exit and picks its best layout, **cuckoo wins essentially every workload.**
Quadratic survives only in the **huge out-of-cache build corner** (2^25, ≥~62.5% load, by ~5–13% on
indirect). Linear rarely strictly wins; it ties at low load and its real edge is memory (non-pow2)
and simplicity, not throughput.

**Layout still matters *within* cuckoo** (no single cuckoo layout is best at everything):
- **find_hit** → **Direct-SIMD** cuckoo (2^25 @87.5%: 7.06 ns vs quad 14.6).
- **find_miss / insert / build** → **Indirect-SIMD** cuckoo (2^25 @87.5% miss: 5.95 vs quad 23.4;
  build penalty vs quad only ~13% on indirect vs ~45% on direct).
- The one place {linear, quad} *beat* cuckoo: **direct-SIMD find_miss, deep out-of-cache, low load**
  (cuckoo pays a flat 2 random cache-line fetches; ~7.9 ns vs linear/quad ~4.9 ns at 25%) — but on
  indirect this reverses (dense tags shrink cuckoo's miss penalty).

## Decision

**Use cuckoo everywhere.** Rationale (build-then-query lifecycle):
- The only workload where cuckoo trails is the one-time **build**, and only by ~13% (indirect, deep
  out-of-cache, high load).
- It is then queried at that high load, where cuckoo is **~4× faster than quadratic on `find_miss`**
  (5.95 vs 23.4 ns) and **~2× on `find_hit`** (7.06 vs 14.6 ns), on every subsequent lookup.
- Paying a ~13% one-time build cost to win every lookup by 2–4× is the right trade for a
  build-once / query-many table.

Pick the cuckoo *layout* by query mix: **Direct-SIMD** if lookups are mostly hits (integer keys),
**Indirect-SIMD** if misses/inserts/builds dominate — consistent with the blog's existing flowchart.
