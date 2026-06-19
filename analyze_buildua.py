#!/usr/bin/env python3
"""build_reserved: does unaligned_cuckoo beat the quadratic tables at high load?
Compares all quad layouts and all cuckoo layouts, median of 3 runs."""
import re, statistics
from collections import defaultdict

MAP = {
    "quadratic_probing_table": "quad_unaln",
    "aligned_quadratic_probing_table": "quad_algn",
    "direct_simd_quadratic_probing": "quad_direct",
    "aligned_cuckoo_table": "cuck_algn",
    "unaligned_cuckoo_table": "cuck_unaln",
    "direct_simd_cuckoo_table": "cuck_direct",
    "linear_probing_table": "lin_indir",
    "direct_simd_linear_probing": "lin_direct",
}
acc = defaultdict(list)
for f in ["results_buildua1.txt", "results_buildua2.txt", "results_buildua3.txt"]:
    size = load = None
    for line in open(f):
        m = re.match(r"mi: 2\^(\d+)", line)
        if m: size = int(m.group(1)); continue
        m = re.match(r"load factor: ([\d.]+)%", line)
        if m: load = float(m.group(1)); continue
        m = re.match(r"build_reserved\s+(\S+?)/(\d+):\s+([\d.]+)", line)
        if m:
            t = m.group(1).split("::")[0]
            if t in MAP: acc[(size, load, MAP[t])].append(float(m.group(3)))
data = {k: statistics.median(v) for k, v in acc.items()}

sizes = [10, 15, 20, 25]
loads = [25.0, 37.5, 50.0, 62.5, 75.0, 87.5]
print("build_reserved (median of 3). Quad family vs Cuckoo family. * = best cuckoo beats best quad.")
print(f"{'size':>4}{'load%':>6} | {'quad_unaln':>10}{'quad_algn':>10}{'quad_direct':>11} | "
      f"{'cuck_algn':>10}{'cuck_unaln':>10}{'cuck_direct':>11} | {'bestC/bestQ':>11}")
print("-"*100)
for size in sizes:
    for load in loads:
        g = lambda k: data.get((size, load, k))
        quads = {k: g(k) for k in ("quad_unaln", "quad_algn", "quad_direct")}
        cucks = {k: g(k) for k in ("cuck_algn", "cuck_unaln", "cuck_direct")}
        bq = min(v for v in quads.values() if v is not None)
        bc = min(v for v in cucks.values() if v is not None)
        ratio = bc / bq
        star = " *CUCKOO WINS" if ratio < 0.97 else ("" if ratio <= 1.03 else "  (quad wins)")
        def f(v): return f"{v:10.2f}" if v is not None else f"{'--':>10}"
        print(f"{size:>4}{load:6.1f} | {f(quads['quad_unaln'])}{f(quads['quad_algn'])}{f(quads['quad_direct']):>11} | "
              f"{f(cucks['cuck_algn'])}{f(cucks['cuck_unaln'])}{f(cucks['cuck_direct']):>11} | {ratio:11.3f}{star}")
    print()

print("="*100)
print("FOCUS: unaligned_cuckoo vs best quadratic, high load (75%, 87.5%):")
for size in sizes:
    for load in (75.0, 87.5):
        cu = data.get((size, load, "cuck_unaln"))
        quads = [data.get((size, load, k)) for k in ("quad_unaln", "quad_algn", "quad_direct")]
        bq = min(v for v in quads if v is not None)
        bqname = min((k for k in ("quad_unaln","quad_algn","quad_direct") if data.get((size,load,k))==bq))
        if cu is None: continue
        verd = "unaligned_cuckoo WINS" if cu < bq*0.97 else ("tie" if cu <= bq*1.03 else "quad wins")
        print(f"  2^{size:<2} {load:5.1f}%: unaligned_cuckoo={cu:.2f}  best_quad={bq:.2f} ({bqname})  -> {verd} ({(cu/bq-1)*100:+.0f}%)")
