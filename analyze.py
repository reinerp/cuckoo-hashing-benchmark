#!/usr/bin/env python3
"""Parse the benchmark output and evaluate the domination claim:
   does min(linear, cuckoo) <= quadratic at every (size, load, op, layout) cell?
Usage: python3 analyze.py results.txt
"""
import sys, re
from collections import defaultdict

TABLE_MAP = {
    "quadratic_probing_table": ("indirect", "quad"),
    "linear_probing_table": ("indirect", "linear"),
    "aligned_cuckoo_table": ("indirect", "cuckoo"),
    "direct_simd_quadratic_probing": ("direct", "quad"),
    "direct_simd_linear_probing": ("direct", "linear"),
    "direct_simd_cuckoo_table": ("direct", "cuckoo"),
    "hashbrown": ("reference", "hashbrown"),
}

def parse(path):
    # data[(size, load, op, layout, strategy)] = ns
    data = {}
    size = load = None
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            m = re.match(r"mi: 2\^(\d+)", line)
            if m:
                size = int(m.group(1)); continue
            m = re.match(r"load factor: ([\d.]+)%", line)
            if m:
                load = float(m.group(1)); continue
            m = re.match(r"(\w+)\s+(\S+?)/(\d+):\s+([\d.]+)\s+ns/op", line)
            if m:
                op, tbl, n, ns = m.group(1), m.group(2), int(m.group(3)), float(m.group(4))
                key_tbl = tbl.split("::")[0]
                if key_tbl not in TABLE_MAP:
                    continue
                layout, strat = TABLE_MAP[key_tbl]
                data[(size, load, op, layout, strat)] = ns
    return data

def main():
    data = parse(sys.argv[1])
    sizes = sorted(set(k[0] for k in data))
    loads = sorted(set(k[1] for k in data))
    ops = ["find_miss", "find_hit", "find_hit_latency", "insert_erase"]
    layouts = ["indirect", "direct"]

    EPS = 0.03  # 3% tolerance: treat within-3% as a tie (benchmark noise floor)

    violations = []  # cells where quad is strictly best of {lin,quad,cuckoo} beyond tolerance
    print(f"{'op':16} {'layout':9} {'size':>5} {'load%':>6} | "
          f"{'linear':>8} {'quad':>8} {'cuckoo':>8} | {'min(L,C)/Q':>11}  verdict")
    print("-"*100)
    for op in ops:
        for layout in layouts:
            for size in sizes:
                for load in loads:
                    lin = data.get((size, load, op, layout, "linear"))
                    quad = data.get((size, load, op, layout, "quad"))
                    cuck = data.get((size, load, op, layout, "cuckoo"))
                    if quad is None:
                        continue
                    cands = [x for x in (lin, cuck) if x is not None]
                    best_lc = min(cands) if cands else float("inf")
                    ratio = best_lc / quad
                    # quad uniquely best if quad beats BOTH lin and cuckoo by > EPS
                    q_unique_best = ratio > 1 + EPS
                    verdict = "QUAD BEST" if q_unique_best else ("dominated" if ratio <= 1 + EPS else "")
                    if q_unique_best:
                        violations.append((op, layout, size, load, lin, quad, cuck, ratio))
                    def f(x): return f"{x:8.2f}" if x is not None else f"{'--':>8}"
                    flag = "  <<< QUAD UNIQUELY BEST" if q_unique_best else ""
                    print(f"{op:16} {layout:9} {size:5} {load:6.1f} | "
                          f"{f(lin)} {f(quad)} {f(cuck)} | {ratio:11.3f}  {verdict}{flag}")
            print()

    print("="*100)
    status = 'HOLDS' if not violations else 'FAILS'
    print(f"DOMINATION SUMMARY (tolerance {EPS*100:.0f}%): {status} — "
          f"{len(violations)} cells where quadratic is uniquely best of (linear, quad, cuckoo)")
    for v in violations:
        op, layout, size, load, lin, quad, cuck, ratio = v
        print(f"  {op} {layout} 2^{size} {load}%: linear={lin} quad={quad} cuckoo={cuck} "
              f"(quad beats best alt by {(ratio-1)*100:.1f}%)")

if __name__ == "__main__":
    main()
