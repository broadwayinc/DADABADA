"""Behaviour tests for postRecords group writes (engine::write::Bulk). Every scenario runs against a fresh
server from the binary given, and prints one JSON line per scenario, so the same run against the old and
the new binary can be compared line by line (record ids differ between runs; everything printed does not).

Usage: python3 bulk_tests.py <dadabada binary> <work dir> [scenario ...]
"""
import http.client, json, os, random, signal, subprocess, sys, threading, time

BIN, WORK = sys.argv[1], os.path.abspath(sys.argv[2])
ONLY = sys.argv[3:]
PORT = 17800
HERE = os.path.dirname(os.path.abspath(__file__))
STATUS = os.path.join(HERE, "..", "..", "status.py")


LIVE = []  # the server of the running scenario, stopped if the scenario fails


class Server:
    def __init__(self, name):
        LIVE[:] = [self]
        self.d = os.path.join(WORK, name)
        subprocess.run(["rm", "-rf", self.d], check=True)
        subprocess.run([BIN, "init", "--port", str(PORT), "--bind", "127.0.0.1", "--data", self.d], check=True, stdout=subprocess.DEVNULL)
        self.start()
        for u, lvl in [("loader", "99"), ("other", "99"), ("low", "03")]:
            subprocess.run([BIN, "admin", "--data", self.d, "user", "add", u, "--level", lvl], input=f"{u}-pass\n", text=True,
                           check=True, stdout=subprocess.DEVNULL)

    def start(self):
        self.p = subprocess.Popen([BIN, "start", "--data", self.d], stdout=open(self.d + ".log", "a"), stderr=subprocess.STDOUT)
        for _ in range(200):
            try:
                c = http.client.HTTPConnection("127.0.0.1", PORT, timeout=1)
                c.request("GET", "/v1")
                c.getresponse().read()
                return
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("server did not start")

    def kill9(self):
        self.p.send_signal(signal.SIGKILL)
        self.p.wait()

    def stop(self):
        subprocess.run([BIN, "stop", "--data", self.d], stdout=subprocess.DEVNULL)
        self.p.wait()

    def settle(self):
        """Waits until the background worker has applied every event (counters)."""
        for _ in range(600):
            out = subprocess.run([sys.executable, STATUS, self.d], capture_output=True, text=True).stdout.split()
            if out and out[1] == "0":
                return int(out[0])
            time.sleep(0.1)
        raise RuntimeError("outbox did not drain")


class Client:
    def __init__(self, user="loader"):
        self.c = http.client.HTTPConnection("127.0.0.1", PORT, timeout=120)
        self.tok = self.call("login", {"username": user, "password": f"{user}-pass"})["token"]

    def call(self, m, body, ok=True):
        h = {"Content-Type": "application/json"}
        if getattr(self, "tok", None):
            h["Authorization"] = "Bearer " + self.tok
        self.c.request("POST", "/v1/" + m, json.dumps(body), h)
        out = json.loads(self.c.getresponse().read())
        if ok and isinstance(out, dict) and "error" in out:
            raise RuntimeError(f"{m}: {out['error']}")
        return out


def plain(n, coll="products", tag="sale", lvl="03", seq=0):
    return {"data": {"seq": seq, "n": n}, "config": {"collection": {"name": coll, "access_level": lvl},
            "index": [{"name": "price", "value": n}], "tags": [tag]}}


def scan(cl, query):
    """Every record a query finds, page by page: (count, ids)."""
    ids, sk = [], None
    while True:
        f = {"limit": 1000}
        if sk:
            f["start_key"] = sk
        out = cl.call("getRecords", {"query": query, "fetch": f})
        ids += [r["record_id"] for r in out["list"]]
        if out.get("end_of_list", True) or not out["list"]:
            return ids
        sk = out["start_key"]


def colls(cl):
    out = cl.call("getCollections", {})
    return {c["collection"]: [c["number_of_records"], c["number_of_records_by_level"]] for c in out["list"]}


def tags(cl, coll):
    out = cl.call("getTags", {"query": {"collection": coll}})
    return {t.get("tag"): t.get("number_of_records") for t in out["list"]}


def result_shape(out):
    """What a postRecords answer says, without the parts that differ from run to run."""
    return {"saved": [r["data"] for r in out["list"]], "failed": [(f["index"], f["code"]) for f in out["failed"]]}


# ---------------------------------------------------------------- scenarios

def s_plain():
    """5 x 1000 plain records: all saved, answered in item order, each findable, counters right."""
    srv = Server("plain"); cl = Client()
    seqs, ids, fails = [], [], 0
    for b in range(5):
        out = cl.call("postRecords", {"items": [plain(b * 1000 + k, seq=b * 1000 + k) for k in range(1000)]})
        fails += len(out["failed"]); seqs += [r["data"]["seq"] for r in out["list"]]; ids += [r["record_id"] for r in out["list"]]
    found = scan(cl, {"collection": "products"})
    by_price = scan(cl, {"collection": "products", "index": {"name": "price", "value": 0, "condition": ">="}})
    by_tag = scan(cl, {"collection": "products", "tag": "sale"})
    one = cl.call("getRecords", {"query": {"record_id": ids[1234]}})["list"][0]["data"]["seq"]
    srv.settle()
    r = {"failed": fails, "in_order": seqs == list(range(5000)), "unique_ids": len(set(ids)) == 5000,
         "found": len(found), "by_price": len(by_price), "by_tag": len(by_tag), "same_set": set(found) == set(ids),
         "get_by_id": one, "collections": colls(cl), "tags": tags(cl, "products")}
    srv.stop(); return r


def s_mixed():
    """One request mixing plain records with every kind of item that must not wait or must fail."""
    srv = Server("mixed"); cl = Client("loader")
    target = cl.call("postRecord", {"data": "target", "config": {"collection": "t", "source": {"referencing_limit": 1}}})["record_id"]
    other = cl.call("postRecord", {"data": "existing", "config": {"collection": "products"}})["record_id"]
    items = [
        plain(1, seq=0),
        {"data": 1, "config": {"collection": "bad name!"}},                       # invalid collection name
        plain(2, seq=2),
        {"data": "ref-a", "config": {"collection": "c", "reference": target}},    # reference: saved the usual way
        {"data": "ref-b", "config": {"collection": "c", "reference": target}},    # over referencing_limit 1
        plain(3, seq=5),
        {"data": "u1", "config": {"collection": "u", "unique_id": "same-u"}},     # unique id
        {"data": "u2", "config": {"collection": "u", "unique_id": "same-u"}},     # same unique id: refused
        plain(4, seq=8),
        {"data": "changed", "config": {"record_id": other}},                       # an update
        {"data": 1, "config": {}},                                                 # no collection
        {"data": 1, "config": {"collection": "products", "access_level": "05"}},  # fine for loader (level 99)
        plain(5, seq=12),
        {"data": "missing", "config": {"collection": "c", "reference": "NOSUCHRECORD0000"}},
        plain(6, seq=14),
    ]
    out = cl.call("postRecords", {"items": items})
    low = Client("low").call("postRecords", {"items": [plain(7, seq=100), {"data": 1, "config": {"collection": "products", "access_level": "05"}},
                                                         plain(8, seq=102)]})
    srv.settle()
    refd = cl.call("getRecords", {"query": {"record_id": target}})["list"][0]["referenced_count"]
    changed = cl.call("getRecords", {"query": {"record_id": other}})["list"][0]["data"]
    r = {"answer": result_shape(out), "low_user": result_shape(low), "target_referenced": refd, "updated": changed,
         "collections": colls(cl)}
    srv.stop(); return r


def s_empty_and_errors():
    """Edge requests: no items, every item failing, a single item, items not a list."""
    srv = Server("edges"); cl = Client()
    r = {"empty": result_shape(cl.call("postRecords", {"items": []})),
         "all_bad": result_shape(cl.call("postRecords", {"items": [{"data": 1, "config": {}}, {"data": 2}, 5]})),
         "one": result_shape(cl.call("postRecords", {"items": [plain(1, seq=1)]})),
         "not_list": cl.call("postRecords", {"items": {"a": 1}}, ok=False).get("error", {}).get("code"),
         "big_body": len(cl.call("postRecords", {"items": [{"data": "x" * 200_000, "config": {"collection": "big"}} for _ in range(20)]})["list"])}
    srv.settle(); r["collections"] = colls(cl)
    srv.stop(); return r


def s_concurrent():
    """4 clients sending 10 x 1000 records while 8 clients save 300 single records each: nothing lost or
    doubled, nobody stuck, counters right."""
    srv = Server("concurrent"); errs, ids, lock = [], [], threading.Lock()

    def bulk(w):
        cl = Client()
        for b in range(10):
            out = cl.call("postRecords", {"items": [plain(k, coll="bulk", seq=w) for k in range(1000)]})
            with lock:
                errs.extend(out["failed"]); ids.extend(r["record_id"] for r in out["list"])

    def single(w):
        cl = Client("other")
        for k in range(300):
            out = cl.call("postRecord", plain(k, coll="single", seq=w))
            with lock:
                ids.append(out["record_id"])

    th = [threading.Thread(target=bulk, args=(w,)) for w in range(4)] + [threading.Thread(target=single, args=(w,)) for w in range(8)]
    t0 = time.time()
    for t in th:
        t.start()
    for t in th:
        t.join(300)
    stuck = sum(t.is_alive() for t in th)
    cl = Client()
    srv.settle()
    r = {"stuck_threads": stuck, "failed": len(errs), "saved": len(ids), "unique": len(set(ids)),
         "found_bulk": len(scan(cl, {"collection": "bulk"})), "found_single": len(scan(cl, {"collection": "single"})),
         "collections": colls(cl)}
    srv.stop(); return r


def s_crash():
    """kill -9 during an import, start again: every record is whole (found the same way by every key), and the
    counters agree with what is stored."""
    srv = Server("crash")

    def bulk(w):
        try:
            cl = Client()
            for b in range(100):
                cl.call("postRecords", {"items": [plain(k, tag=f"t{k % 7}", seq=w) for k in range(1000)]})
        except Exception:
            pass

    th = [threading.Thread(target=bulk, args=(w,)) for w in range(4)]
    for t in th:
        t.start()
    time.sleep(3)
    srv.kill9()
    for t in th:
        t.join()
    srv.start()
    cl = Client()
    stored = srv.settle()
    found = scan(cl, {"collection": "products"})
    by_price = scan(cl, {"collection": "products", "index": {"name": "price", "value": 0, "condition": ">="}})
    by_tags = sum(len(scan(cl, {"collection": "products", "tag": f"t{i}"})) for i in range(7))
    cs = colls(cl).get("products", [0, {}])
    tg = tags(cl, "products")
    r = {"some_saved": len(found) > 0, "record_order_eq_price": len(found) == len(by_price), "eq_tags": len(found) == by_tags,
         "unique": len(set(found)) == len(found), "counter_eq_found": cs[0] == len(found), "catalog_eq_found": stored == len(found),
         "tag_counters_eq": sum(v for v in tg.values() if v) == len(found)}
    srv.stop(); return r


SCEN = {"plain": s_plain, "mixed": s_mixed, "edges": s_empty_and_errors, "concurrent": s_concurrent, "crash": s_crash}
for name, f in SCEN.items():
    if ONLY and name not in ONLY:
        continue
    try:
        print(json.dumps({"scenario": name, **f()}, sort_keys=True), flush=True)
    except Exception as e:
        print(json.dumps({"scenario": name, "ERROR": repr(e)}), flush=True)
        for srv in LIVE:
            if srv.p.poll() is None:
                srv.kill9()
