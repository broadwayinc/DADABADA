"""Combines repeated runs of bench.py into one results-<size>.json per size: for every cell and database, the
median of the runs' medians (and of their p95s). The laptop's speed changes between runs by up to a third,
so one run is not enough; the median of three keeps a slow or fast run from deciding a number.
Usage: python3 combine.py runs/   (reads runs/results-<size>-r<n>.json, writes results-<size>.json here)
"""
import glob, json, os, re, statistics, sys

d = sys.argv[1]
sizes = sorted({re.match(r"results-(.+)-r\d+\.json", os.path.basename(f)).group(1) for f in glob.glob(os.path.join(d, "results-*-r*.json"))})
for s in sizes:
    runs = [json.load(open(f)) for f in sorted(glob.glob(os.path.join(d, f"results-{s}-r*.json")))]
    base = runs[0]
    out = {**base, "repeats": len(runs), "cells": []}
    for i, c in enumerate(base["cells"]):
        cs = [r["cells"][i] for r in runs]
        assert all((x["kind"], x["level"]) == (c["kind"], c["level"]) for x in cs)
        assert not any(any(x["mismatches"].values()) or any(x["full_mismatches"].values()) for x in cs), (s, c["kind"], c["level"])
        times = {n: {k: statistics.median(x["times"][n][k] for x in cs) for k in ("median", "p95", "mean")} for n in c["times"]}
        out["cells"].append({**c, "times": times, "per_run_medians": {n: [round(x["times"][n]["median"], 2) for x in cs] for n in c["times"]}})
    json.dump(out, open(f"results-{s}.json", "w"), indent=1)
    print(f"{s}: {len(runs)} runs combined")
