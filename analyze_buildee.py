#!/usr/bin/env python3
"""build_reserved with cuckoo EARLY-EXIT vs without; and does best-cuckoo now beat best-quad?
buildee* = early-exit on (aligned & direct cuckoo); buildua* = early-exit off."""
import re, statistics
from collections import defaultdict

MAP = {
    "quadratic_probing_table": "quad_unaln", "aligned_quadratic_probing_table": "quad_algn",
    "direct_simd_quadratic_probing": "quad_direct",
    "aligned_cuckoo_table": "cuck_algn", "unaligned_cuckoo_table": "cuck_unaln",
    "direct_simd_cuckoo_table": "cuck_direct",
}
def load(files):
    acc = defaultdict(list)
    for f in files:
        size = loadf = None
        try: fh = open(f)
        except FileNotFoundError: continue
        for line in fh:
            m = re.match(r"mi: 2\^(\d+)", line)
            if m: size = int(m.group(1)); continue
            m = re.match(r"load factor: ([\d.]+)%", line)
            if m: loadf = float(m.group(1)); continue
            m = re.match(r"build_reserved\s+(\S+?)/(\d+):\s+([\d.]+)", line)
            if m:
                t = m.group(1).split("::")[0]
                if t in MAP: acc[(size, loadf, MAP[t])].append(float(m.group(3)))
    return {k: statistics.median(v) for k, v in acc.items()}

ee = load(["results_buildee1.txt","results_buildee2.txt","results_buildee3.txt"])  # early-exit ON
ua = load(["results_buildua1.txt","results_buildua2.txt","results_buildua3.txt"])  # early-exit OFF

sizes = [10,15,20,25]; loads=[25.0,37.5,50.0,62.5,75.0,87.5]
print("CUCKOO EARLY-EXIT effect on build_reserved (ns/op). algn/direct: OFF -> ON; unaln unchanged.")
print(f"{'size':>4}{'load':>6} | {'algn_off':>9}{'algn_on':>9} | {'direct_off':>11}{'direct_on':>10} | {'unaln':>8}")
print("-"*70)
for size in sizes:
    for load_ in loads:
        def g(d,k): return d.get((size,load_,k))
        a_off, a_on = g(ua,'cuck_algn'), g(ee,'cuck_algn')
        d_off, d_on = g(ua,'cuck_direct'), g(ee,'cuck_direct')
        un = g(ee,'cuck_unaln')
        f=lambda v: f"{v:8.2f}" if v is not None else f"{'--':>8}"
        print(f"{size:>4}{load_:6.1f} | {f(a_off)} {f(a_on)} | {f(d_off):>11} {f(d_on)} | {f(un)}")
    print()

print("="*78)
print("NEW best-cuckoo (early-exit ON) vs best-quad on build_reserved, high load:")
print(f"{'size':>4}{'load':>6} | {'best_cuckoo(which)':>22} {'best_quad(which)':>20} | verdict")
print("-"*78)
for size in sizes:
    for load_ in [50.0,62.5,75.0,87.5]:
        cucks = {k:ee.get((size,load_,k)) for k in ("cuck_algn","cuck_unaln","cuck_direct")}
        quads = {k:ee.get((size,load_,k)) for k in ("quad_unaln","quad_algn","quad_direct")}
        cucks={k:v for k,v in cucks.items() if v}; quads={k:v for k,v in quads.items() if v}
        if not cucks or not quads: continue
        bc=min(cucks,key=cucks.get); bq=min(quads,key=quads.get)
        r=cucks[bc]/quads[bq]
        verd = "CUCKOO WINS" if r<0.97 else ("tie" if r<=1.03 else "quad wins")
        print(f"{size:>4}{load_:6.1f} | {cucks[bc]:7.2f} ({bc:>12}) {quads[bq]:7.2f} ({bq:>10}) | {verd} ({(r-1)*100:+.0f}%)")
