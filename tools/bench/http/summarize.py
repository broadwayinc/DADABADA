"""Turns results-10k.json, results-100k.json and results-1m.json (from bench.py, all over HTTP) into perf.json
for the landing page. Usage: python3 summarize.py <perf.json> <sqlite version> <postgres version> <mongodb version>"""
import json, os, re, sys

OUT, LITE_V, PG_V, MONGO_V = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
CARGO = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "Cargo.toml")
VERSION = re.search(r'^version = "(.+)"', open(CARGO).read(), re.M).group(1)
SHARE = {0: "0.1%", 1: "1%", 2: "10%", 3: "100%"}
KEY = {"DDBD": "dd", "PostgreSQL": "pg", "SQLite": "lite", "MongoDB": "mongo"}


def load(p):
    r = json.load(open(p))
    cells = {}
    for c in r["cells"]:
        assert all(m == 0 for m in c["mismatches"].values()), (p, c["kind"], c["level"], c["mismatches"])
        assert all(m == 0 for m in c["full_mismatches"].values()), (p, c["kind"], c["level"], c["full_mismatches"])
        t = {}
        for name, v in c["times"].items():
            t[KEY[name]] = round(v["median"], 2)
            t[KEY[name] + "_p95"] = round(v["p95"], 2)
        cells[(c["kind"], c["level"])] = {"rows": round(c["avg_rows"], 1), **t}
    return r, cells


sizes = [("10k", 10_000), ("100k", 100_000), ("1m", 1_000_000)]
data = {s: load(f"results-{s}.json") for s, _ in sizes}
r1m, c1m = data["1m"]
out = {
    "engines": {"dd": f"DDBD {VERSION}", "pg": f"PostgreSQL {PG_V}", "lite": f"SQLite {LITE_V}", "mongo": f"MongoDB {MONGO_V}"},
    "transport": "http",
    "runs": r1m["runs"],
    "repeats": r1m.get("repeats", 1),
    "limit": r1m["limit"],
    "table": [{"kind": k, "level": f"{L:02}", "share": SHARE[L], **c1m[(k, L)]} for k in ["tag", "range"] for L in [0, 1, 2, 3]],
    "scaling": {str(L): [{"records": n, **data[s][1][("tag", L)]} for s, n in sizes] for L in [0, 1, 2, 3]},
}
json.dump(out, open(OUT, "w"), indent=1)
for r in out["table"]:
    print(f"{r['kind']:5} {r['share']:>5}  rows {r['rows']:5.1f}  dd {r['dd']:6.2f}  pg {r['pg']:6.2f}  lite {r['lite']:6.2f}  mongo {r['mongo']:6.2f}")
