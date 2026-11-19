#!/usr/bin/env python3
"""Analyse ab.py output against a base variant.

usage: an.py DATA.json BASE [--markdown] [--exclude NAME ...] [--noise NAME] [--gate]

Without --gate the script only reports. With --gate it exits 1 when any reported
variant's verdict is SLOWER or INCONCLUSIVE (the single-thread no-regression gate).

Round statistic = median of a variant's decodes in that round (no minima).
Reports p10/p50/p90 of all decodes, the median paired ratio (variant/base) over rounds,
a bootstrap 95% CI of that median (5000 resamples, fixed seed) and a two-sided sign test.
Verdict: FASTER if CI upper < 0; PASS if CI upper <= bound; SLOWER if CI lower > 0;
otherwise INCONCLUSIVE. bound = +0.5%, or with --noise NAME (an A/A build of BASE) the
CI upper of NAME vs BASE on the same case of this run, if that is larger.
"""
import json, math, random, statistics as st, sys

data = json.load(open(sys.argv[1]))
base = sys.argv[2]
md = "--markdown" in sys.argv
excluded = {sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--exclude"}
gate = "--gate" in sys.argv
noise = next((sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--noise"), None)
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


def compare(rr, b):
    ratios = [st.median(r) / y for r, y in zip(rr, b)]
    boot = sorted(st.median(random.choices(ratios, k=len(ratios))) for _ in range(5000))
    k = sum(r < 1 for r in ratios)
    return st.median(ratios), boot[125], boot[4874], k, sum(r != 1 for r in ratios)


def bound_for(vs):
    if not noise:
        return 1.005
    return max(1.005, compare(vs[noise], [st.median(r) for r in vs[base]])[2])


if md:
    print("| case | variant | p10 / p50 / p90 ms | vs " + base + " | 95% CI | faster | sign p | verdict |")
    print("|---|---|---|---|---|---|---|---|")
for case, vs in data.items():
    b = [st.median(r) for r in vs[base]]
    allb = [x for r in vs[base] for x in r]
    bound = bound_for(vs)
    if md:
        print(f"| {case} | {base} | {pct(allb,10):.2f} / {pct(allb,50):.2f} / {pct(allb,90):.2f} | | bound +{100*(bound-1):.2f}% | | | |")
    else:
        print(f"## {case}  ({base}: p10/p50/p90 {pct(allb,10):.2f} {pct(allb,50):.2f} {pct(allb,90):.2f} ms, rounds={len(b)}, bound +{100*(bound-1):.2f}%)")
    for v, rr in vs.items():
        if v == base or v in excluded:
            continue
        allv = [x for r in rr for x in r]
        med, lo, hi, k, n = compare(rr, b)
        verdict = "FASTER" if hi < 1 else "PASS" if hi <= bound else "SLOWER" if lo > 1 else "INCONCLUSIVE"
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
