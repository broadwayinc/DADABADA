"""Write benchmark: DADABADA, PostgreSQL and SQLite, all over HTTP with the same requests (see ../README.md
and ../gateway), into empty tables whose indexes already exist, with the same durability (a write survives
a crash of the server, not of the operating system).

  batch:  N records, 1,000 per postRecords request, from 4 clients at once; one database at a time. After
          each, the time until the database's background work after the load has finished is measured,
          and the next database starts only then.
  single: 3,000 postRecord requests of one record each, one at a time, the three databases in turn.
  update: 3,000 postRecord requests that change the price (an index) and data of an existing record.

The records are the read benchmark's: price 1..1,000,000, one of 20 tags, access level 00-03.
Usage: python3 bench.py <N> <dd port> <pg port> <sqlite port> <dd pid> <pgdata> <out.json>
"""
import http.client, json, os, random, statistics, subprocess, sys, threading, time

N, PORTS, DD_PID, PGDATA, OUT = int(sys.argv[1]), [int(x) for x in sys.argv[2:5]], int(sys.argv[5]), sys.argv[6], sys.argv[7]
NAMES = ["DADABADA", "PostgreSQL", "SQLite"]
TAGS = ["sale"] + [f"tag{i:02}" for i in range(1, 20)]
HERE = os.path.dirname(os.path.abspath(__file__))
BATCH, CLIENTS = 1000, 4
SINGLE = int(os.environ.get("SINGLE", 3000))


def rec(rng, i):
    p = rng.randint(1, 1_000_000)
    r = rng.random()
    lvl = "00" if r < 0.001 else "01" if r < 0.01 else "02" if r < 0.10 else "03"
    return {"data": {"title": f"Product {i}", "price": p},
            "config": {"collection": {"name": "products", "access_level": lvl}, "index": [{"name": "price", "value": p}],
                       "tags": [TAGS[rng.randrange(20)]]}}


class Http:
    def __init__(self, port):
        self.conn = http.client.HTTPConnection("127.0.0.1", port)
        self.tok = self.call("login", {"username": "loader", "password": "loader-pass"})["token"]

    def call(self, method, body, tok=None):
        h = {"Content-Type": "application/json"}
        if tok or getattr(self, "tok", None):
            h["Authorization"] = "Bearer " + (tok or self.tok)
        self.conn.request("POST", "/v1/" + method, json.dumps(body), h)
        out = json.loads(self.conn.getresponse().read())
        if "error" in out:
            raise RuntimeError(f"{method}: {out['error']}")
        return out


def settle(name):
    """Seconds until the database has finished its background work after a load."""
    t0 = time.time()
    if name == "DADABADA":
        subprocess.run([sys.executable, os.path.join(HERE, "..", "idle.py"), str(DD_PID)], check=True, stdout=subprocess.DEVNULL)
        return round(time.time() - t0 - 20, 1)  # idle.py waits for 20 quiet seconds
    if name == "PostgreSQL":
        sys.path.insert(0, os.path.join(HERE, "..", "..", "pylib"))
        import pgserver, psycopg
        with psycopg.connect(pgserver.get_server(PGDATA, cleanup_mode=None).get_uri(), autocommit=True) as c:
            c.execute("CHECKPOINT")
        return round(time.time() - t0, 1)
    return 0.0  # SQLite has no background work; its WAL is checkpointed as it goes


def batch_phase(name, port):
    batches = []
    rng = random.Random(42)
    for b in range(N // BATCH):
        batches.append([rec(rng, b * BATCH + k) for k in range(BATCH)])
    ids, lat, lock, collisions = [None] * len(batches), [], threading.Lock(), []
    nxt = iter(range(len(batches)))

    def worker():
        h = Http(port)
        while True:
            with lock:
                b = next(nxt, None)
            if b is None:
                return
            t0 = time.perf_counter()
            out = h.call("postRecords", {"items": batches[b]})
            dt = time.perf_counter() - t0
            got = [r["record_id"] for r in out["list"]]
            # a record id that came out the same as an existing one (DADABADA: EXISTS) is sent again and counted
            while out["failed"]:
                assert all(f["code"] == "EXISTS" for f in out["failed"]), out["failed"][:2]
                with lock:
                    collisions.append(len(out["failed"]))
                out = h.call("postRecords", {"items": [batches[b][f["index"]] for f in out["failed"]]})
                got += [r["record_id"] for r in out["list"]]
            with lock:
                ids[b] = got
                lat.append(dt * 1000)

    t0 = time.time()
    th = [threading.Thread(target=worker) for _ in range(CLIENTS)]
    for t in th:
        t.start()
    for t in th:
        t.join()
    took = time.time() - t0
    after = settle(name)
    lat.sort()
    r = {"records": N, "seconds": round(took, 1), "per_s": round(N / took), "batch_ms_median": round(statistics.median(lat), 1),
         "batch_ms_p95": round(lat[int(len(lat) * 0.95) - 1], 1), "settle_s": after, "id_collisions": sum(collisions)}
    print(f"batch  {name:10} {N:,} records in {took:6.1f} s = {N / took:8,.0f}/s  median request {r['batch_ms_median']} ms  "
          f"background work after: {after} s  id collisions resent: {sum(collisions)}", flush=True)
    return r, [i for b in ids for i in b]


def timed(engines, make, label):
    lat = {n: [] for n in NAMES}
    for i in range(SINGLE):
        for k in range(3):
            n = NAMES[(i + k) % 3]
            body = make(n, i)
            t0 = time.perf_counter()
            engines[n].call("postRecord", body)
            lat[n].append((time.perf_counter() - t0) * 1000)
    out = {}
    for n, t in lat.items():
        t.sort()
        out[n] = {"median": round(statistics.median(t), 3), "p95": round(t[int(len(t) * 0.95) - 1], 3), "mean": round(statistics.fmean(t), 3)}
    print(f"{label:6} " + "  ".join(f"{n}: {v['median']:.2f} ms (p95 {v['p95']:.2f})" for n, v in out.items()), flush=True)
    return out


def main():
    res = {"records": N, "batch": {}, "single": None, "update": None}
    ids = {}
    for n, port in zip(NAMES, PORTS):
        res["batch"][n], ids[n] = batch_phase(n, port)
    engines = {n: Http(p) for n, p in zip(NAMES, PORTS)}
    rng = random.Random(5)
    singles = [rec(rng, N + i) for i in range(SINGLE)]
    res["single"] = timed(engines, lambda n, i: singles[i], "single")
    for n in NAMES:
        settle(n)
    picks = [rng.randrange(N) for _ in range(SINGLE)]
    prices = [rng.randint(1, 1_000_000) for _ in range(SINGLE)]
    res["update"] = timed(engines, lambda n, i: {"data": {"title": f"Product {picks[i]}", "price": prices[i]},
                                                  "config": {"record_id": ids[n][picks[i]], "index": [{"name": "price", "value": prices[i]}]}}, "update")
    json.dump(res, open(OUT, "w"), indent=1)


if __name__ == "__main__":
    main()
