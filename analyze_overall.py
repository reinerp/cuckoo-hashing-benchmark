#!/usr/bin/env python3
"""Overall picture: best-of-layouts winner among linear / quadratic / cuckoo, per workload.
Reads results_all{1,2,3}.txt (all 5 workloads in one file). Median of 3."""
import re, statistics
from collections import defaultdict

# table name -> (strategy, layout)
MAP = {
    "quadratic_probing_table": ("quad", "indir-unaln"),
    "aligned_quadratic_probing_table": ("quad", "indir-algn"),
    "direct_simd_quadratic_probing": ("quad", "direct"),
    "linear_probing_table": ("linear", "indir"),
    "direct_simd_linear_probing": ("linear", "direct"),
    "aligned_cuckoo_table": ("cuckoo", "indir-algn"),
    "unaligned_cuckoo_table": ("cuckoo", "indir-unaln"),
    "direct_simd_cuckoo_table": ("cuckoo", "direct"),
}
acc = defaultdict(list)
for f in ["results_all1.txt", "results_all2.txt", "results_all3.txt"]:
    size = load = None
    try: fh = open(f)
    except FileNotFoundError: continue
    for line in fh:
        m = re.match(r"mi: 2\^(\d+)", line)
        if m: size = int(m.group(1)); continue
        m = re.match(r"load factor: ([\d.]+)%", line)
        if m: load = float(m.group(1)); continue
        m = re.match(r"(\w+)\s+(\S+?)/(\d+):\s+([\d.]+)\s+ns/op", line)
        if m:
            op, t = m.group(1), m.group(2).split("::")[0]
            if t in MAP:
                strat, lay = MAP[t]
                acc[(op, size, load, strat, lay)].append(float(m.group(4)))
data = {k: statistics.median(v) for k, v in acc.items()}

WL = [("find_hit","find_hit"),("find_miss","find_miss"),("find_hit_latency","latency(in-cache)"),
      ("insert_erase","insert (churn)"),("build_reserved","build (reserved)")]
sizes = [10,15,20,25]; loads=[25.0,37.5,50.0,62.5,75.0,87.5]
EPS = 0.04

def best(op, size, load, strat):
    vals = [(data[k], k[4]) for k in data if k[0]==op and k[1]==size and k[2]==load and k[3]==strat]
    return min(vals) if vals else (None, None)

wintotals = defaultdict(lambda: defaultdict(int))
print("WINNER GRID  (L=linear  Q=quad  C=cuckoo;  lowercase=within 4% tie of winner)")
for op,label in WL:
    print(f"\n#### {label} ####")
    print(f"{'sz/ld%':>9} " + "".join(f"{l:>7.1f}" for l in loads))
    for size in sizes:
        cells=[]
        for load in loads:
            b = {s: best(op,size,load,s)[0] for s in ("linear","quad","cuckoo")}
            b = {s:v for s,v in b.items() if v is not None}
            if not b: cells.append("  -  "); continue
            win = min(b, key=b.get); wv=b[win]
            within = [s for s,v in b.items() if v <= wv*(1+EPS)]
            letter = {"linear":"L","quad":"Q","cuckoo":"C"}[win]
            # mark ties: if 2+ within tolerance, show winner upper + others lower
            tie = len(within) > 1
            cells.append(f"{letter}{'~' if tie else ' '}    ")
            wintotals[op][win]+=1
        print(f"{('2^'+str(size)):>9} " + "".join(f"{c:>7}" for c in cells))

print("\n" + "="*70)
print("WIN COUNTS per workload (strict winner of best-of-layouts; 24 cells each, 12 for latency):")
for op,label in WL:
    t=wintotals[op]
    tot=sum(t.values())
    print(f"  {label:18}: " + ", ".join(f"{s}={t.get(s,0)}" for s in ("linear","quad","cuckoo")) + f"   (of {tot})")

print("\n" + "="*70)
print("DETAIL: best-of-layouts ns/op (winner in [brackets]) at representative cells")
for op,label in WL:
    print(f"\n-- {label} --")
    for size in (10,25):
        for load in (25.0,50.0,87.5):
            row=[]
            for s in ("linear","quad","cuckoo"):
                v,lay=best(op,size,load,s)
                row.append((s,v,lay))
            row=[r for r in row if r[1] is not None]
            if not row: continue
            win=min(row,key=lambda r:r[1])
            cell=" ".join(f"{'['+r[0][0].upper()+']' if r is win else ' '+r[0][0].upper()+' '}{r[1]:6.2f}({r[2][:3]})" for r in row)
            print(f"   2^{size:<2} {load:5.1f}%: {cell}")
