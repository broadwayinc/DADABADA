# Benchmark over HTTP: DADABADA, PostgreSQL and SQLite through the same kind of request

The first benchmark (`../`) measured DADABADA over HTTP, PostgreSQL over its local socket with psycopg,
and SQLite inside the Python client. So DADABADA's time included an HTTP server and full JSON records,
and the others' did not. This run puts every database behind HTTP with the same API shape.

## What is the same for all three

- **The request.** One Python client, one kept-alive HTTP connection per database, the same
  `POST /v1/getRecords` JSON body with `Authorization: Bearer <token>`. The time runs from the request
  sent to the answer decoded.
- **The server around the database.** PostgreSQL and SQLite sit behind `sqlgw` (`gateway/`), a small Rust
  server built with the same libraries as DADABADA's (hyper 1, tokio, serde_json, TCP_NODELAY). Per
  request it does what DADABADA does: it parses the JSON body, hashes the token with SHA-256 and looks it
  up in an in-memory session table to find the caller's level, runs the query on a blocking thread, and
  builds and serializes the JSON answer.
- **The answer.** The SQL tables hold each record's full JSON exactly as DADABADA returned it when it was
  saved, so all three send the same records, byte for byte after decoding. `bench.py` checks the record
  ids of every answer and the whole decoded answer on a sample.
- **Where the database runs.** SQLite runs inside `sqlgw`, as RocksDB runs inside DADABADA. PostgreSQL is
  its own process, reached over its local socket, as any application server reaches it.
- **Order.** The three are queried in turn, query by query, in a rotating order, so a change in the
  machine's speed during the run reaches all three alike.

`sqlgw` writes no request log; DADABADA writes one line per request. That small cost stays on DADABADA's side.

## The access levels (what 0.1%, 1%, 10%, 100% mean)

Each record gets an access level when it is made: 00 on 0.1% of the records, 01 on 0.9%, 02 on 9% and
03 on 90%. A user may read the records at their own level or below. So the users `u00`, `u01`, `u02` and
`u03` may read 0.1%, 1%, 10% and 100% of the records. Every query is run once as each of them.

## Run

From this folder, with `cargo build --release` done in the repository root and the Python packages in
`../pylib` (see `../README.md`):

```bash
$ for s in "10000 10k" "100000 100k" "1000000 1m"; do ./run.sh $s /path/to/workdir; done
$ mkdir -p runs; for r in 1 2 3; do                       # three runs of the queries on the loaded data
>   for s in "10000 10k" "100000 100k" "1000000 1m"; do QUERIES_ONLY=1 ./run.sh $s /path/to/workdir
>     set -- $s; cp results-$2.json runs/results-$2-r$r.json; done; done
$ python3 combine.py runs/                                 # median of the runs, per cell and database
$ python3 summarize.py ../../../../docs-dadabada/.vitepress/theme/landing/perf.json 3.46.0 16.2 8.0.4
```

`run.sh` loads all four databases, lets them settle (DADABADA restarted and left idle, PostgreSQL
checkpointed and analyzed), starts the three gateways and runs `bench.py`, writing `results-<name>.json`.

## MongoDB (fourth database in the read benchmark)

MongoDB 8.0.4 Community runs from its official server tarball next to the scripts, with no system install
(checksum from the `.sha256` file published beside the tarball):

```bash
$ curl -LO https://fastdl.mongodb.org/linux/mongodb-linux-x86_64-ubuntu2404-8.0.4.tgz
$ tar xzf mongodb-linux-x86_64-ubuntu2404-8.0.4.tgz
$ mkdir -p ../mongobin && cp mongodb-linux-x86_64-ubuntu2404-8.0.4/bin/mongod ../mongobin/
$ python3 -m pip install --target ../pylib "pymongo==4.8.0"
```

`run.sh` starts it with a 2 GB cache (as PostgreSQL's shared buffers) on a local socket and loads it with
`load_mongo.py`: one document per record (`_id`, collection, level, price, tags and the record as a
sub-document), with the same indexes as PostgreSQL. The gateway answers a query with one `find`, sorted by
price and `_id`. `QUERIES_ONLY=1 ./run.sh ...` reruns the queries on DADABADA, PostgreSQL and SQLite data
loaded by an earlier run (MongoDB is loaded again).
