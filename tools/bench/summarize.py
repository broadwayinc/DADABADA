"""Turns results-10k.json, results-100k.json and results-1m.json into perf.json for the landing page."""
import json, os, re, sys

OUT = sys.argv[1]
# the version measured is the one in Cargo.toml, which the release build in target/ was made from
CARGO = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "Cargo.toml")
VERSION = re.search(r'^version = "(.+)"', open(CARGO).read(), re.M).group(1)
SHARE = {0: "0.1%", 1: "1%", 2: "10%", 3: "100%"}
NAMES = {"DADABADA": "dd", "PostgreSQL 16": "pg"}


def load(p):
    r = json.load(open(p))
    cells = {}
    for c in r["cells"]:
        t = {}
        for name, v in c["times"].items():
            key = NAMES.get(name, "lite")
            t[key] = round(v["median"], 2)
            t[key + "_p95"] = round(v["p95"], 2)
        assert all(m == 0 for m in c["mismatches"].values()), c["mismatches"]
        cells[(c["kind"], c["level"])] = {"rows": round(c["avg_rows"], 1), **t}
    return r, cells


sizes = [("10k", 10_000), ("100k", 100_000), ("1m", 1_000_000)]
data = {s: load(f"results-{s}.json") for s, _ in sizes}
r1m, c1m = data["1m"]
out = {
    "engines": {"dd": f"DADABADA {VERSION}", "pg": r1m["engines"][1].replace("PostgreSQL 16", "PostgreSQL 16.2"), "lite": r1m["engines"][2]},
    "runs": r1m["runs"],
    "limit": r1m["limit"],
    "table": [
        {"kind": k, "level": f"{L:02}", "share": SHARE[L], **c1m[(k, L)]}
        for k in ["tag", "range"] for L in [0, 1, 2, 3]
    ],
    "scaling": {
        str(L): [{"records": n, **data[s][1][("tag", L)]} for s, n in sizes] for L in [0, 1, 2, 3]
    },
}
json.dump(out, open(OUT, "w"), indent=1)
print(json.dumps(out["table"], indent=0)[:1500])
