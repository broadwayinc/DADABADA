"""Loads the benchmark data set into a running DDBD server and writes rows.tsv (id, price, level, tag,
record JSON) for the SQL loaders, so all three databases hold exactly the same records with the same ids,
and the SQL gateways can answer with the same full record JSON DDBD answers with.

Data set (seeded, so every run makes the same records):
  N products, price = random integer 1..1,000,000, one tag out of 20 (one of them is "sale", 5%),
  access level: "00" for 0.1%, "01" for 0.9%, "02" for 9%, "03" for 90%.
  So a caller at level 00 can read 0.1% of the records, 01 -> 1%, 02 -> 10%, 03 -> 100%.
"""
import json, random, sys, threading, time, http.client, queue

HOST, PORT = "127.0.0.1", int(sys.argv[2]) if len(sys.argv) > 2 else 17100
N = int(sys.argv[1]) if len(sys.argv) > 1 else 1_000_000
BATCH = 1000
TAGS = ["sale"] + [f"tag{i:02}" for i in range(1, 20)]


def records():
    rng = random.Random(42)
    for i in range(N):
        price = rng.randint(1, 1_000_000)
        r = rng.random()
        level = "00" if r < 0.001 else "01" if r < 0.01 else "02" if r < 0.10 else "03"
        tag = TAGS[rng.randrange(20)]
        yield i, price, level, tag


def call(conn, method, body, token=None):
    h = {"Content-Type": "application/json"}
    if token:
        h["Authorization"] = "Bearer " + token
    conn.request("POST", "/v1/" + method, json.dumps(body), h)
    r = conn.getresponse()
    out = json.loads(r.read())
    if "error" in out:
        raise RuntimeError(out["error"])
    return out


def main():
    batches = queue.Queue(maxsize=16)
    results = {}
    lock = threading.Lock()

    def worker():
        conn = http.client.HTTPConnection(HOST, PORT)
        tok = call(conn, "login", {"username": "loader", "password": "loader-pass"})["token"]
        while True:
            item = batches.get()
            if item is None:
                return
            n, rows = item
            items = [{"data": {"title": f"Product {i}", "price": p},
                      "config": {"collection": {"name": "products", "access_level": lvl},
                                 "index": [{"name": "price", "value": p}], "tags": [tag]}}
                     for i, p, lvl, tag in rows]
            out = call(conn, "postRecords", {"items": items}, tok)
            assert not out["failed"], out["failed"][:2]
            with lock:
                results[n] = [(r["record_id"], p, lvl, tag, json.dumps(r, separators=(",", ":"), sort_keys=True))
                              for r, (i, p, lvl, tag) in zip(out["list"], rows)]

    threads = [threading.Thread(target=worker) for _ in range(4)]
    for t in threads:
        t.start()
    t0 = time.time()
    rows, n = [], 0
    for rec in records():
        rows.append(rec)
        if len(rows) == BATCH:
            batches.put((n, rows))
            n, rows = n + 1, []
            if n % 100 == 0:
                print(f"{n * BATCH:>9,} records  {n * BATCH / (time.time() - t0):,.0f}/s", flush=True)
    if rows:
        batches.put((n, rows))
    for _ in threads:
        batches.put(None)
    for t in threads:
        t.join()
    took = time.time() - t0
    with open(sys.argv[3] if len(sys.argv) > 3 else "rows.tsv", "w") as f:
        for k in sorted(results):
            for rid, p, lvl, tag, rec in results[k]:
                f.write(f"{rid}\t{p}\t{int(lvl)}\t{tag}\t{rec}\n")
    print(f"loaded {N:,} records in {took:.0f} s ({N / took:,.0f}/s)")


if __name__ == "__main__":
    main()
