#!/usr/bin/env python3
"""Paired, mirrored-order (ABBA...) decode benchmark. Keeps every measurement.

usage: ab.py BINDIR OUT.json ROUNDS MEAS WARM variants case [case ...]
  variants: comma list of name[:threads], e.g. dev,reg,thr:1,thr:4
  case:     label=file:unsafe(0|1):colorspace(rgb|luma|rgba|bgr|bgra)

Per round and case, every variant runs in order v1..vk then vk..v1; each invocation is a
fresh process doing WARM discarded + MEAS measured decodes.
"""
import json, os, subprocess, sys, time

bindir, out = sys.argv[1], sys.argv[2]


def bounded(text, lo, hi, what):
    if not text.isdigit() or not lo <= int(text) <= hi:
        sys.exit(f"{what} must be an integer in {lo}..{hi}, got {text!r}")
    return int(text)


rounds = bounded(sys.argv[3], 1, 100, "rounds")
meas = bounded(sys.argv[4], 1, 20, "decodes per invocation")
warm = bounded(sys.argv[5], 0, 5, "warm-up decodes")
variants = sys.argv[6].split(",")
cases = {}
for c in sys.argv[7:]:
    label, spec = c.split("=", 1)
    f, u, cs = spec.rsplit(":", 2)
    cases[label] = (f, u, cs)
exe = ".exe" if os.name == "nt" else ""
data = {label: {v: [] for v in variants} for label in cases}
for r in range(rounds):
    for label, (f, u, cs) in cases.items():
        acc = {v: [] for v in variants}
        for v in variants + variants[::-1]:
            name, _, threads = v.partition(":")
            res = subprocess.run(
                [os.path.join(bindir, f"hx_{name}{exe}"), "bench", f, u, cs, str(warm), str(meas)],
                env=dict(os.environ, HX_THREADS=threads or "1"),
                capture_output=True, text=True, check=True,
            )
            acc[v] += [float(x) for x in res.stdout.split()]
        for v in variants:
            data[label][v].append(acc[v])
    with open(out, "w") as fh:
        json.dump(data, fh)
    print("round", r, time.strftime("%X"), flush=True)
