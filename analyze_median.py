#!/usr/bin/env python3
"""Median-combine multiple benchmark runs and evaluate the domination claim.
Usage: python3 analyze_median.py results_run1.txt results_run2.txt ...
"""
import sys, re, statistics
from collections import defaultdict

TABLE_MAP = {
    "quadratic_probing_table": ("indirect", "quad"),
    "linear_probing_table": ("indirect", "linear"),
    "aligned_cuckoo_table": ("indirect", "cuckoo"),
    "direct_simd_quadratic_probing": ("direct", "quad"),
    "direct_simd_linear_probing": ("direct", "linear"),
    "direct_simd_linear_probing_np2": ("direct", "linear_np2"),
    "direct_simd_cuckoo_table": ("direct", "cuckoo"),
    "hashbrown": ("reference", "hashbrown"),
}

def parse(path, acc):
    size = load = None
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            m = re.match(r"mi: 2\^(\d+)", line)
            if m: size = int(m.group(1)); continue
            m = re.match(r"load factor: ([\d.]+)%", line)
            if m: load = float(m.group(1)); continue
            m = re.match(r"(\w+)\s+(\S+?)/(\d+):\s+([\d.]+)\s+ns/op", line)
            if m:
                op, tbl, n, ns = m.group(1), m.group(2), int(m.group(3)), float(m.group(4))
                key_tbl = tbl.split("::")[0]
                if key_tbl not in TABLE_MAP: continue
                layout, strat = TABLE_MAP[key_tbl]
                acc[(size, load, op, layout, strat)].append(ns)

def main():
    files = sys.argv[1:]
    acc = defaultdict(list)
    for p in files: parse(p, acc)
    data = {k: statistics.median(v) for k, v in acc.items()}

    sizes = sorted(set(k[0] for k in data))
    loads = sorted(set(k[1] for k in data))
    ops = ["find_miss", "find_hit", "find_hit_latency", "insert_erase"]
    layouts = ["indirect", "direct"]
    EPS = 0.04  # 4% tie tolerance (benchmark noise floor on medians of 3)

    violations = []
    print(f"# Median of {len(files)} runs. EPS={EPS*100:.0f}% tie tolerance.")
    print(f"{'op':16} {'layout':9} {'size':>4} {'load%':>6} | "
          f"{'linear':>8} {'quad':>8} {'cuckoo':>8} | {'min(L,C)/Q':>10}  verdict")
    print("-"*98)
    for op in ops:
        for layout in layouts:
            for size in sizes:
                for load in loads:
                    lin = data.get((size, load, op, layout, "linear"))
                    quad = data.get((size, load, op, layout, "quad"))
                    cuck = data.get((size, load, op, layout, "cuckoo"))
                    if quad is None: continue
                    cands = [x for x in (lin, cuck) if x is not None]
                    best_lc = min(cands) if cands else float("inf")
                    ratio = best_lc / quad
                    q_unique_best = ratio > 1 + EPS
                    if q_unique_best:
                        violations.append((op, layout, size, load, lin, quad, cuck, ratio))
                    def f(x): return f"{x:8.2f}" if x is not None else f"{'--':>8}"
                    verdict = "QUAD BEST <<<" if q_unique_best else "dominated"
                    print(f"{op:16} {layout:9} {size:4} {load:6.1f} | "
                          f"{f(lin)} {f(quad)} {f(cuck)} | {ratio:10.3f}  {verdict}")
            print()

    print("="*98)
    status = 'HOLDS (no cell where quad uniquely best)' if not violations else f'FAILS at {len(violations)} cells'
    print(f"DOMINATION (min(linear,cuckoo) <= quad, tol {EPS*100:.0f}%): {status}")
    by_op = defaultdict(list)
    for v in violations: by_op[v[0]].append(v)
    for op in ops:
        if by_op[op]:
            print(f"\n  -- {op}: {len(by_op[op])} quad-best cells --")
            for op_, layout, size, load, lin, quad, cuck, ratio in by_op[op]:
                cs = f"{cuck:.2f}" if cuck is not None else "N/A"
                print(f"     {layout:9} 2^{size:<2} {load:5.1f}%: linear={lin:.2f} quad={quad:.2f} "
                      f"cuckoo={cs}  (quad better by {(ratio-1)*100:.0f}%)")

    # ---- mul_high tax: direct linear (mask) vs linear_np2 (mulhi), same size/load ----
    print("\n" + "="*98)
    print("NON-POWER-OF-2 TAX: direct-SIMD linear pow2(&mask) vs np2(mul_high), same layout/load")
    print(f"{'op':16} {'size':>4} {'load%':>6} | {'lin(mask)':>9} {'lin_np2':>9} | {'np2/mask':>9}")
    print("-"*60)
    taxes = []
    for op in ["find_miss", "find_hit", "insert_erase"]:
        for size in sizes:
            for load in loads:
                a = data.get((size, load, op, "direct", "linear"))
                b = data.get((size, load, op, "direct", "linear_np2"))
                if a and b:
                    print(f"{op:16} {size:4} {load:6.1f} | {a:9.2f} {b:9.2f} | {b/a:9.3f}")
                    if a > 0.5:  # skip sub-ns noise
                        taxes.append(b/a)
            print()
    if taxes:
        print(f"  median np2/mask ratio (excl. sub-ns cells): {statistics.median(taxes):.3f} "
              f"=> mul_high tax ~ {(statistics.median(taxes)-1)*100:.0f}%")

if __name__ == "__main__":
    main()
