"""Runs the same getRecords queries against DADABADA, PostgreSQL 16, SQLite and MongoDB, every one of them over HTTP, and
writes results.json. PostgreSQL and SQLite sit behind sqlgw (gateway/), which serves the same API shape as
DADABADA: same URL, same JSON request, bearer token, same full record JSON in the answer.

  range: products with price >= X, cheapest first, first page of 50
  tag:   products tagged "sale" with price >= X, cheapest first, first page of 50

each as a caller at level 00, 01, 02 and 03. Records were given an access level when they were made
(00 on 0.1% of them, 01 on 0.9%, 02 on 9%, 03 on 90%), and a caller may read records at their own level or
below, so these callers may read 0.1%, 1%, 10% and 100% of the records.

For every (query, level) the same 300 random X values go to each database after 30 warm-up queries, the
four interleaved query by query (in a rotating order), so any drift of the machine reaches all four alike.
The time is measured in one Python client with one kept-alive connection per database, request sent to
answer decoded. Every answer is compared across the four: same record ids in the same order, and on a
sample the whole decoded answer.

Usage: python3 bench.py <dd port> <pg gateway port> <sqlite gateway port> <mongodb gateway port> <out.json> [kinds] [levels]
"""
import http.client, json, random, statistics, sys, time

dd_port, pg_port, lite_port, mongo_port, out_path = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
KINDS = sys.argv[6].split(",") if len(sys.argv) > 6 else ["range", "tag"]
LEVELS = [int(x) for x in sys.argv[7].split(",")] if len(sys.argv) > 7 else [0, 1, 2, 3]
RUNS, WARM, LIMIT, FULL_CHECK = 300, 30, 50, 20


class Http:
    def __init__(self, name, port):
        self.name = name
        self.conn = http.client.HTTPConnection("127.0.0.1", port)
        self.tok = {L: self.call("login", {"username": f"u{L:02}", "password": f"u{L:02}-pass"})["token"] for L in LEVELS}

    def call(self, method, body, tok=None):
        h = {"Content-Type": "application/json"}
        if tok:
            h["Authorization"] = "Bearer " + tok
        self.conn.request("POST", "/v1/" + method, json.dumps(body), h)
        out = json.loads(self.conn.getresponse().read())
        if "error" in out:
            raise RuntimeError(f"{self.name}: {out['error']}")
        return out

    def query(self, kind, L, x):
        q = {"collection": "products", "index": {"name": "price", "value": x, "condition": ">="}}
        if kind == "tag":
            q["tag"] = "sale"
        return self.call("getRecords", {"query": q, "fetch": {"limit": LIMIT}}, self.tok[L])


def main():
    engines = [Http("DADABADA", dd_port), Http("PostgreSQL", pg_port), Http("SQLite", lite_port), Http("MongoDB", mongo_port)]
    rng = random.Random(7)
    res = {"engines": [e.name for e in engines], "runs": RUNS, "limit": LIMIT, "transport": "http", "cells": []}
    for kind in KINDS:
        for L in LEVELS:
            xs = [rng.randint(1, 1_000_000) for _ in range(RUNS)]
            warm = [rng.randint(1, 1_000_000) for _ in range(WARM)]
            for x in warm:
                for e in engines:
                    e.query(kind, L, x)
            ts = {e.name: [] for e in engines}
            ids = {e.name: [] for e in engines}
            full_bad = {e.name: 0 for e in engines}
            for i, x in enumerate(xs):
                answers = {}
                for k in range(len(engines)):
                    e = engines[(i + k) % len(engines)]
                    t0 = time.perf_counter()
                    out = e.query(kind, L, x)
                    ts[e.name].append((time.perf_counter() - t0) * 1000)
                    ids[e.name].append([r["record_id"] for r in out["list"]])
                    answers[e.name] = out["list"]
                if i < FULL_CHECK:
                    for n, a in answers.items():
                        full_bad[n] += a != answers["DADABADA"]
            cell = {"kind": kind, "level": L, "times": {}}
            for n, t in ts.items():
                t.sort()
                cell["times"][n] = {"median": statistics.median(t), "p95": t[int(len(t) * 0.95) - 1], "mean": statistics.fmean(t)}
            base = ids["DADABADA"]
            cell["mismatches"] = {n: sum(a != b for a, b in zip(base, ids[n])) for n in ids}
            cell["full_mismatches"] = full_bad
            cell["avg_rows"] = statistics.fmean(len(a) for a in base)
            res["cells"].append(cell)
            line = "  ".join(f"{n}: {v['median']:7.2f} ms (p95 {v['p95']:7.2f})" for n, v in cell["times"].items())
            print(f"{kind:5} level {L:02}  rows {cell['avg_rows']:5.1f}  {line}  id mismatches {cell['mismatches']}  "
                  f"full mismatches {full_bad}", flush=True)
    json.dump(res, open(out_path, "w"), indent=1)


if __name__ == "__main__":
    main()
