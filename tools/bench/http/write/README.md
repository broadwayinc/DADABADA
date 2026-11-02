# Write benchmark: DDBD, PostgreSQL and SQLite over HTTP

How fast each database saves records when all three get the same HTTP requests (see `../README.md` for
the gateway). The records are the read benchmark's: a price index, one of 20 tags, an access level.

## What is the same for all three

- **Requests:** `postRecords` with 1,000 records, `postRecord` with one, and `postRecord` with
  `config.record_id` to change a record's price (an index) and data. Every answer is the full saved
  record(s) as JSON, as DDBD answers.
- **Indexes in place:** the SQL tables are created empty with all the read benchmark's indexes, so every
  insert updates them, as in a running app. (A bulk load with indexes built at the end would be faster
  for PostgreSQL and SQLite, and is not what an app does.)
- **Durability:** a saved write survives a crash of the database server but not of the operating
  system, which is DDBD's guarantee (`docs-dadabada/admin/operations.md`). PostgreSQL runs with
  `synchronous_commit = off` (fsync stays on), SQLite in WAL mode with `synchronous = NORMAL`.
- **PostgreSQL's fast path:** a batch is one multi-row `INSERT ... SELECT FROM unnest(...)`, not 1,000
  separate statements. SQLite runs prepared inserts in one transaction.

## Phases

1. **batch:** N records, 1,000 per request, from 4 clients at once, one database at a time. After each,
   the time until its background work after the load has finished (DDBD sorting its new data into
   place, PostgreSQL's checkpoint) is measured, and the next database starts only then.
2. **single:** 3,000 one-record requests, one at a time, the three databases in turn.
3. **update:** 3,000 price changes of existing records, the three databases in turn.

## Run

```bash
$ ./run.sh 1000000 /path/to/workdir
```

It writes `results.json` and prints the record counts and data sizes of all three at the end.

## Behaviour tests for grouped writes

`bulk_tests.py` checks what postRecords does when it saves plain new records together in one write
(`engine::write::Bulk`). It runs each scenario on a fresh server and prints one JSON line per scenario,
so a run on an older binary and one on a newer binary can be compared with `diff`:

```bash
$ python3 bulk_tests.py <old ddbd binary> /tmp/t-old > old.jsonl
$ python3 bulk_tests.py <new ddbd binary> /tmp/t-new > new.jsonl
$ diff old.jsonl new.jsonl
```

Scenarios: `plain` (5,000 records: order of the answer, every key, counters), `mixed` (plain records
among references, a reference limit, unique ids, updates, refused items and a lower-level user), `edges`
(empty, all refused, one item, large bodies), `concurrent` (4 importing clients and 8 clients saving single
records at once: nothing lost, doubled or stuck), `crash` (kill -9 during an import: every record whole,
counters equal to what is stored).
