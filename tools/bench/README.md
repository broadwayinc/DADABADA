# Benchmark: DADABADA, PostgreSQL and SQLite

> The landing page now shows the run in `http/`, where PostgreSQL and SQLite also answer over HTTP with the
> same request and the same full records as DADABADA. The steps below are the earlier run, kept for reference.

The numbers on the docs landing page come from these scripts. All three databases get the same records
and answer the same queries; every answer is compared across the three.

## What you need

- A release build: `cargo build --release` (the scripts use `target/release/dadabada`).
- Python 3 with PostgreSQL and its driver installed next to the scripts (no system install):

```bash
$ cd tools/bench
$ python3 -m pip install --target pylib pgserver "psycopg[binary]"
```

`pgserver` runs a private PostgreSQL 16 from a folder. SQLite comes with Python.

## Steps

Run everything from `tools/bench`. `<n>` is the number of records (10000, 100000 or 1000000).

1. Start a DADABADA server for the data set and add the users the scripts sign in as:

```bash
$ B=../../target/release/dadabada
$ $B init --port 17100 --bind 127.0.0.1 --data dd
$ $B start --data dd &
$ echo loader-pass | $B admin --data dd user add loader --level 99
$ for l in 00 01 02 03; do echo "u$l-pass" | $B admin --data dd user add u$l --level $l; done
```

2. Load the records into DADABADA. This also writes `rows.tsv`, the record ids for the SQL databases:

```bash
$ python3 load_dd.py <n> 17100 rows.tsv
```

3. Make sure the server's background queue is empty (the second number), so the queries do not
   compete with it. It keeps up with the load, so this is normally already 0:

```bash
$ python3 status.py dd
```

4. Load the same rows into PostgreSQL (data folder `pgdata`) and SQLite (`bench.sqlite`):

```bash
$ python3 load_sql.py rows.tsv pgdata bench.sqlite records
```

5. Let the databases settle, so none is still busy with the load while it answers: restart DADABADA
   (`$B stop --data dd`, then `$B start --data dd &`) and wait until its process is idle, which after a
   million records takes a minute or two while RocksDB sorts the new data into place; then run
   `CHECKPOINT` and `VACUUM ANALYZE records` in PostgreSQL.

6. Run the queries and write the results:

```bash
$ python3 bench.py 17100 pgdata bench.sqlite results-1m.json records
```

For the 10,000 and 100,000 sets, use another port and data folder for DADABADA, another SQLite file,
and the table names `records_10k` and `records_100k`, writing `results-10k.json` and `results-100k.json`.
`python3 summarize.py perf.json` then combines the three files into the data the landing page shows
(`doc-dadabada/.vitepress/theme/landing/perf.json`).
