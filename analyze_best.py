#!/usr/bin/env python3
"""Best-of-layouts comparison: for each workload x (size,load), reduce each STRATEGY to the better
of its two layouts (direct vs indirect), then compare best-linear vs best-quadratic vs best-cuckoo.

Lookup/churn ops come from results_run{1,2,3}.txt; build_reserved from results_build{1,2,3}.txt.
"""
import re, statistics
from collections import defaultdict

MAP = {
    "quadratic_probing_table": ("indirect", "quad"),
    "linear_probing_table": ("indirect", "linear"),
    "aligned_cuckoo_table": ("indirect", "cuckoo"),
    "direct_simd_quadratic_probing": ("direct", "quad"),
    "direct_simd_linear_probing": ("direct", "linear"),
    "direct_simd_cuckoo_table": ("direct", "cuckoo"),
}
RUN_FILES = ["results_run1.txt", "results_run2.txt", "results_run3.txt"]
BUILD_FILES = ["results_build1.txt", "results_build2.txt", "results_build3.txt"]

def parse(files, acc):
    for path in files:
        size = load = None
        for line in open(path):
            m = re.match(r"mi: 2\^(\d+)", line)
            if m: size = int(m.group(1)); continue
            m = re.match(r"load factor: ([\d.]+)%", line)
            if m: load = float(m.group(1)); continue
            m = re.match(r"(\w+)\s+(\S+?)/(\d+):\s+([\d.]+)\s+ns/op", line)
            if m:
                op, tbl, ns = m.group(1), m.group(2).split("::")[0], float(m.group(4))
                if tbl in MAP:
                    lay, st = MAP[tbl]
                    acc[(op, size, load, lay, st)].append(ns)

acc = defaultdict(list)
parse(RUN_FILES, acc)
parse(BUILD_FILES, acc)
data = {k: statistics.median(v) for k, v in acc.items()}

WORKLOADS = [
    ("find_hit", "find_hit"), ("find_miss", "find_miss"),
    ("find_hit_latency", "find_hit_latency"),
    ("insert_erase", "insert (churn @ load)"), ("build_reserved", "build (into reserved)"),
]
sizes = [10, 15, 20, 25]
loads = [25.0, 37.5, 50.0, 62.5, 75.0, 87.5]
EPS = 0.04

def best(op, size, load, strat):
    """Return (value, layout) for the better layout of this strategy, or (None, None)."""
    cands = []
    for lay in ("indirect", "direct"):
        v = data.get((op, size, load, lay, strat))
        if v is not None:
            cands.append((v, lay))
    if not cands:
        return (None, None)
    return min(cands)

quad_best_cells = defaultdict(list)
for op, label in WORKLOADS:
    print(f"\n{'='*92}\nWORKLOAD: {label}   (best-of-layouts per strategy; L/Q/C = which layout won: i=indirect d=direct)")
    print(f"{'size':>4} {'load%':>6} | {'linear':>11} {'quad':>11} {'cuckoo':>11} | {'min(L,C)/Q':>10}  verdict")
    print("-"*92)
    for size in sizes:
        for load in loads:
            lv, ll = best(op, size, load, "linear")
            qv, ql = best(op, size, load, "quad")
            cv, cl = best(op, size, load, "cuckoo")
            if qv is None: continue
            alt = [x for x in (lv, cv) if x is not None]
            bestlc = min(alt) if alt else float("inf")
            ratio = bestlc / qv
            quad_unique = ratio > 1 + EPS
            if quad_unique:
                quad_best_cells[op].append((size, load, lv, ll, qv, ql, cv, cl, ratio))
            def fmt(v, l):
                return f"{v:7.2f}({l[0]})" if v is not None else f"{'--':>10}"
            verdict = "QUAD BEST <<<" if quad_unique else "dominated"
            print(f"{size:>4} {load:6.1f} | {fmt(lv,ll)} {fmt(qv,ql)} {fmt(cv,cl)} | {ratio:10.3f}  {verdict}")
        print()

print("="*92)
total = sum(len(v) for v in quad_best_cells.values())
print(f"BEST-OF-LAYOUTS DOMINATION (min(best_linear, best_cuckoo) <= best_quad, tol {EPS*100:.0f}%):")
print(f"  {'HOLDS — quad never uniquely best' if total==0 else f'FAILS at {total} cells'}")
for op, label in WORKLOADS:
    cells = quad_best_cells[op]
    if cells:
        print(f"\n  -- {label}: {len(cells)} cells where best-quad beats best-linear AND best-cuckoo --")
        for size, load, lv, ll, qv, ql, cv, cl, ratio in cells:
            print(f"     2^{size:<2} {load:5.1f}%:  best_quad={qv:.2f}({ql})  "
                  f"best_linear={lv:.2f}({ll})  best_cuckoo={cv:.2f}({cl})  (quad better by {(ratio-1)*100:.0f}%)")
