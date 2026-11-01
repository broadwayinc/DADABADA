"""Loads rows.tsv (from load_dd.py) into MongoDB as a schemaless app would keep these records: one document
per record with _id (the record id), collection, level, price, tags and the record itself as a sub-document
(the same JSON DADABADA returns). Indexes as in PostgreSQL, one for each thing the queries filter or sort on:
(collection, price, _id), (collection, level, price, _id) and tags.
Usage: python3 load_mongo.py <rows.tsv> <mongodb uri>
"""
import json, os, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "pylib"))
from pymongo import ASCENDING, MongoClient  # noqa: E402

rows_path, uri = sys.argv[1], sys.argv[2]
db = MongoClient(uri).bench
db.records.drop()
t0, batch, n = time.time(), [], 0
with open(rows_path) as f:
    for line in f:
        rid, price, level, tag, rec = line.rstrip("\n").split("\t")
        batch.append({"_id": rid, "collection": "products", "level": int(level), "price": int(price), "tags": [tag],
                      "rec": json.loads(rec)})
        if len(batch) == 5000:
            db.records.insert_many(batch, ordered=False); n += len(batch); batch = []
if batch:
    db.records.insert_many(batch, ordered=False); n += len(batch)
db.records.create_index([("collection", ASCENDING), ("price", ASCENDING), ("_id", ASCENDING)])
db.records.create_index([("collection", ASCENDING), ("level", ASCENDING), ("price", ASCENDING), ("_id", ASCENDING)])
db.records.create_index([("tags", ASCENDING)])
db.client.admin.command("fsync")
st = db.command("collstats", "records")
print(f"MongoDB loaded {n:,} records in {time.time() - t0:.0f} s, {(st['storageSize'] + st['totalIndexSize']) / 1e6:.0f} MB on disk",
      flush=True)
