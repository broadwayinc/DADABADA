"""Loads rows.tsv (written by load_dd.py) into PostgreSQL 16 and SQLite, as a schemaless app would store
the same records: a generic records table with the JSON body, plus an index on every field the benchmark
queries use. Usage: python3 load_sql.py <rows.tsv> <pgdata dir> <sqlite file>
"""
import io, json, os, sys, time, sqlite3

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "pylib"))
import pgserver, psycopg  # noqa: E402

rows_path, pgdata, lite_path = sys.argv[1], sys.argv[2], sys.argv[3]
TABLE = sys.argv[4] if len(sys.argv) > 4 else "records"


def rows():
    with open(rows_path) as f:
        for i, line in enumerate(f):
            rid, price, level, tag = line.rstrip("\n").split("\t")
            yield rid, int(price), int(level), tag, json.dumps({"title": f"Product {i}", "price": int(price)})


# ---------- PostgreSQL ----------
t0 = time.time()
conf = os.path.join(pgdata, "postgresql.conf")
if os.path.exists(conf) and "# bench settings" not in open(conf).read():
    with open(conf, "a") as f:
        # generous to PostgreSQL: a buffer cache that holds the whole data set, no JIT for short queries,
        # and a plan made for each query's actual values
        f.write("\n# bench settings\nshared_buffers = 2GB\njit = off\nplan_cache_mode = force_custom_plan\n")
srv = pgserver.get_server(pgdata, cleanup_mode=None)
with psycopg.connect(srv.get_uri(), autocommit=True) as c:
    c.execute(f"DROP TABLE IF EXISTS {TABLE}")
    c.execute(f"""CREATE TABLE {TABLE} (
        id text COLLATE "C" PRIMARY KEY, collection text NOT NULL, level smallint NOT NULL,
        price bigint NOT NULL, tags text[] NOT NULL, data jsonb NOT NULL)""")
    with c.cursor().copy(f"COPY {TABLE} (id, collection, level, price, tags, data) FROM STDIN") as cp:
        for rid, price, level, tag, data in rows():
            cp.write_row((rid, "products", level, price, [tag], data))
    c.execute(f"CREATE INDEX {TABLE}_coll_price ON {TABLE} (collection, price, id)")
    c.execute(f"CREATE INDEX {TABLE}_coll_level_price ON {TABLE} (collection, level, price, id)")
    c.execute(f"CREATE INDEX {TABLE}_tags ON {TABLE} USING gin (tags)")
    c.execute(f"VACUUM ANALYZE {TABLE}")
    size = c.execute(f"SELECT pg_size_pretty(pg_total_relation_size('{TABLE}'))").fetchone()[0]
print(f"PostgreSQL loaded in {time.time() - t0:.0f} s, {size}", flush=True)

# ---------- SQLite ----------
t0 = time.time()
if os.path.exists(lite_path):
    os.remove(lite_path)
db = sqlite3.connect(lite_path)
db.executescript("""
PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;
CREATE TABLE records (id TEXT PRIMARY KEY, collection TEXT NOT NULL, level INTEGER NOT NULL,
                      price INTEGER NOT NULL, data TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE record_tags (tag TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY (tag, id)) WITHOUT ROWID;
""")
batch, tags = [], []
for rid, price, level, tag, data in rows():
    batch.append((rid, "products", level, price, data))
    tags.append((tag, rid))
    if len(batch) == 50000:
        db.executemany("INSERT INTO records VALUES (?,?,?,?,?)", batch)
        db.executemany("INSERT INTO record_tags VALUES (?,?)", tags)
        batch, tags = [], []
db.executemany("INSERT INTO records VALUES (?,?,?,?,?)", batch)
db.executemany("INSERT INTO record_tags VALUES (?,?)", tags)
db.commit()
db.executescript("""
CREATE INDEX records_coll_price ON records (collection, price, id);
CREATE INDEX records_coll_level_price ON records (collection, level, price, id);
ANALYZE;
""")
db.close()
print(f"SQLite loaded in {time.time() - t0:.0f} s, {os.path.getsize(lite_path) / 1e6:.0f} MB", flush=True)
