"""Creates the empty tables for the write benchmark, with every index already in place (as an app's tables
are when records arrive): the same schema and indexes as the read benchmark.
Usage: python3 schema.py <pgdata> <sqlite file>"""
import os, sqlite3, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "pylib"))
import pgserver, psycopg  # noqa: E402

pgdata, lite = sys.argv[1], sys.argv[2]
with psycopg.connect(pgserver.get_server(pgdata, cleanup_mode=None).get_uri(), autocommit=True) as c:
    c.execute("DROP TABLE IF EXISTS records")
    c.execute("""CREATE TABLE records (id text COLLATE "C" PRIMARY KEY, collection text NOT NULL, level smallint NOT NULL,
        price bigint NOT NULL, tags text[] NOT NULL, data jsonb NOT NULL)""")
    c.execute("CREATE INDEX records_coll_price ON records (collection, price, id)")
    c.execute("CREATE INDEX records_coll_level_price ON records (collection, level, price, id)")
    c.execute("CREATE INDEX records_tags ON records USING gin (tags)")
if os.path.exists(lite):
    os.remove(lite)
db = sqlite3.connect(lite)
db.executescript("""
PRAGMA journal_mode = WAL;
CREATE TABLE records (id TEXT PRIMARY KEY, collection TEXT NOT NULL, level INTEGER NOT NULL,
                      price INTEGER NOT NULL, data TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE record_tags (tag TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY (tag, id)) WITHOUT ROWID;
CREATE INDEX records_coll_price ON records (collection, price, id);
CREATE INDEX records_coll_level_price ON records (collection, level, price, id);
""")
db.close()
print("empty tables with indexes ready")
