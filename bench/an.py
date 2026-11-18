#!/usr/bin/env python3
"""Analyse ab.py output against a base variant.

usage: an.py DATA.json BASE [--markdown] [--exclude NAME ...] [--gate]

Without --gate the script only reports. With --gate it exits 1 when any reported
variant's verdict is SLOWER or INCONCLUSIVE (the single-thread no-regression gate).

Round statistic = median of a variant's decodes in that round (no minima).
Reports p10/p50/p90 of all decodes, the median paired ratio (variant/base) over rounds,
a bootstrap 95% CI of that median (5000 resamples, fixed seed) and a two-sided sign test.
Verdict (predeclared): FASTER if CI upper < 0; PASS if CI upper <= +0.5%;
SLOWER if CI lower > 0; otherwise INCONCLUSIVE.
"""
import json, math, random, statistics as st, sys

data = json.load(open(sys.argv[1]))
base = sys.argv[2]
md = "--markdown" in sys.argv
excluded = {sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--exclude"}
gate = "--gate" in sys.argv
failed = []
random.seed(7)


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p / 100 * len(v)))]


def sign_p(k, n):
    if n == 0:
        return 1.0
    t = sum(math.comb(n, i) for i in range(0, min(k, n - k) + 1)) / 2**n
    return min(1.0, 2 * t)


if md:
    print("| case | variant | p10 / p50 / p90 ms | vs " + base + " | 95% CI | faster | sign p | verdict |")
    print("|---|---|---|---|---|---|---|---|")
for case, vs in data.items():
    b = [st.median(r) for r in vs[base]]
    allb = [x for r in vs[base] for x in r]
    if md:
        print(f"| {case} | {base} | {pct(allb,10):.2f} / {pct(allb,50):.2f} / {pct(allb,90):.2f} | | | | | |")
    else:
        print(f"## {case}  ({base}: p10/p50/p90 {pct(allb,10):.2f} {pct(allb,50):.2f} {pct(allb,90):.2f} ms, rounds={len(b)})")
    for v, rr in vs.items():
        if v == base or v in excluded:
            continue
        a = [st.median(r) for r in rr]
        allv = [x for r in rr for x in r]
        ratios = [x / y for x, y in zip(a, b)]
        med = st.median(ratios)
        boot = sorted(st.median(random.choices(ratios, k=len(ratios))) for _ in range(5000))
        lo, hi = boot[125], boot[4874]
        k = sum(r < 1 for r in ratios)
        n = sum(r != 1 for r in ratios)
        verdict = "FASTER" if hi < 1 else "PASS" if hi <= 1.005 else "SLOWER" if lo > 1 else "INCONCLUSIVE"
        dist = f"{pct(allv,10):.2f} / {pct(allv,50):.2f} / {pct(allv,90):.2f}"
        ci = f"[{100*(lo-1):+.2f}, {100*(hi-1):+.2f}]"
        if md:
            print(f"| {case} | {v} | {dist} | {100*(med-1):+.2f}% | {ci} | {k}/{n} | {sign_p(k,n):.2g} | {verdict} |")
        else:
            print(f"  {v:10s} {dist} | {100*(med-1):+6.2f}% CI95 {ci} faster {k}/{n} p={sign_p(k,n):.2g} -> {verdict}")
        if verdict not in ("FASTER", "PASS"):
            failed.append(f"{case}/{v}: {verdict}")
if gate and failed:
    print("GATE FAILED: " + ", ".join(failed), file=sys.stderr)
    sys.exit(1)
