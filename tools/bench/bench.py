"""Runs the same queries on DADABADA, PostgreSQL 16 and SQLite and writes results.json.

  range: products with price >= X, cheapest first, first page of 50
  tag:   products tagged "sale" with price >= X, cheapest first, first page of 50

each as a caller at level 00, 01, 02 and 03 (who may read 0.1%, 1%, 10% and 100% of the records).
For every (query, level) the same 300 random X values are sent to each database after 30 warm-up queries;
the time is measured in one Python client, request to decoded answer. Every answer is compared across the
three databases: same record ids in the same order. Usage: python3 bench.py <dd port> <pgdata> <sqlite> <out.json>
"""
import http.client, json, os, random, statistics, sqlite3, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "pylib"))
import pgserver, psycopg  # noqa: E402

port, pgdata, lite_path, out_path = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
TABLE = sys.argv[5] if len(sys.argv) > 5 else "records"
KINDS = sys.argv[6].split(",") if len(sys.argv) > 6 else ["range", "tag"]
LEVELS = [int(x) for x in sys.argv[7].split(",")] if len(sys.argv) > 7 else [0, 1, 2, 3]
RUNS, WARM, LIMIT = 300, 30, 50


class Dadabada:
    name = "DADABADA"

    def __init__(self):
        self.conn = http.client.HTTPConnection("127.0.0.1", port)
        self.tok = {L: self._call("login", {"username": f"u{L:02}", "password": f"u{L:02}-pass"})["token"] for L in LEVELS}

    def _call(self, method, body, tok=None):
        h = {"Content-Type": "application/json"}
        if tok:
            h["Authorization"] = "Bearer " + tok
        self.conn.request("POST", "/v1/" + method, json.dumps(body), h)
        out = json.loads(self.conn.getresponse().read())
        if "error" in out:
            raise RuntimeError(out["error"])
        return out

    def query(self, kind, L, x):
        q = {"collection": "products", "index": {"name": "price", "value": x, "condition": ">="}}
        if kind == "tag":
            q["tag"] = "sale"
        out = self._call("getRecords", {"query": q, "fetch": {"limit": LIMIT}}, self.tok[L])
        return [r["record_id"] for r in out["list"]]


PG_SQL = {
    "range": "SELECT id, data FROM {T} WHERE collection = 'products' AND price >= %s AND level <= %s ORDER BY price, id LIMIT 50",
    "tag": "SELECT id, data FROM {T} WHERE collection = 'products' AND price >= %s AND level <= %s AND tags @> ARRAY['sale'] ORDER BY price, id LIMIT 50",
}


class Postgres:
    name = "PostgreSQL 16"

    def __init__(self):
        self.srv = pgserver.get_server(pgdata, cleanup_mode=None)
        self.conn = psycopg.connect(self.srv.get_uri(), autocommit=True)

    def query(self, kind, L, x):
        return [r[0] for r in self.conn.execute(PG_SQL[kind].replace("{T}", TABLE), (x, L), prepare=True).fetchall()]

    def plan(self, kind, L, x):
        sql = "EXPLAIN (ANALYZE, BUFFERS) " + PG_SQL[kind].replace("{T}", TABLE)
        return [r[0] for r in self.conn.execute(sql, (x, L)).fetchall()]


LITE_SQL = {
    "range": "SELECT id, data FROM {T} WHERE collection = 'products' AND price >= ? AND level <= ? ORDER BY price, id LIMIT 50",
    "tag": "SELECT id, data FROM {T} WHERE collection = 'products' AND price >= ? AND level <= ? AND id IN (SELECT id FROM record_tags WHERE tag = 'sale') ORDER BY price, id LIMIT 50",
}


class Lite:
    name = "SQLite " + sqlite3.sqlite_version

    def __init__(self):
        self.db = sqlite3.connect(lite_path)
        self.db.execute("PRAGMA cache_size = -2000000")  # 2 GB page cache: the whole file stays in memory
        self.db.execute("PRAGMA mmap_size = 4000000000")

    def query(self, kind, L, x):
        return [(r[0], json.loads(r[1]))[0] for r in self.db.execute(LITE_SQL[kind].replace("{T}", "records"), (x, L)).fetchall()]

    def plan(self, kind, L, x):
        return [str(r[3]) for r in self.db.execute("EXPLAIN QUERY PLAN " + LITE_SQL[kind].replace("{T}", "records"), (x, L)).fetchall()]


def main():
    engines = [Dadabada(), Postgres(), Lite()]
    rng = random.Random(7)
    res = {"engines": [e.name for e in engines], "runs": RUNS, "limit": LIMIT, "cells": [], "plans": {}}
    for kind in KINDS:
        for L in LEVELS:
            xs = [rng.randint(1, 1_000_000) for _ in range(RUNS)]
            warm = [rng.randint(1, 1_000_000) for _ in range(WARM)]
            answers, cell = {}, {"kind": kind, "level": L, "times": {}}
            for e in engines:
                for x in warm:
                    e.query(kind, L, x)
                ts, ans = [], []
                for x in xs:
                    t0 = time.perf_counter()
                    ids = e.query(kind, L, x)
                    ts.append((time.perf_counter() - t0) * 1000)
                    ans.append(ids)
                answers[e.name] = ans
                ts.sort()
                cell["times"][e.name] = {"median": statistics.median(ts), "p95": ts[int(len(ts) * 0.95) - 1],
                                         "mean": statistics.fmean(ts)}
            base = answers[engines[0].name]
            cell["mismatches"] = {n: sum(a != b for a, b in zip(base, answers[n])) for n in answers}
            cell["avg_rows"] = statistics.fmean(len(a) for a in base)
            res["cells"].append(cell)
            line = "  ".join(f"{n}: {v['median']:7.2f} ms (p95 {v['p95']:7.2f})" for n, v in cell["times"].items())
            print(f"{kind:5} level {L:02}  rows {cell['avg_rows']:5.1f}  {line}  mismatches {cell['mismatches']}", flush=True)
            res["plans"][f"{kind}-{L}"] = {"pg": engines[1].plan(kind, L, 500_000), "sqlite": engines[2].plan(kind, L, 500_000)}
    json.dump(res, open(out_path, "w"), indent=1)


if __name__ == "__main__":
    main()
