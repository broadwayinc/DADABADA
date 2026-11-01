#!/usr/bin/env bash
# One data set, start to finish: load all four databases, let them settle, put PostgreSQL, SQLite and MongoDB
# behind sqlgw, and run bench.py with every database over HTTP.
# Usage: ./run.sh <records> <name> <work dir>     e.g. ./run.sh 1000000 1m /data/bench
#   writes results-<name>.json next to this script
#   QUERIES_ONLY=1 ./run.sh ...  reuses the loaded DADABADA, PostgreSQL and SQLite data of an earlier run (only
#   MongoDB, whose data folder is made again with each load, is loaded again) and runs the queries once more
set -euo pipefail
N=$1; NAME=$2; W=$(realpath -m "$3")/$NAME
HERE=$(cd "$(dirname "$0")" && pwd)
B=$HERE/../../../target/release/dadabada
GW=$HERE/gateway/target/release/sqlgw
DDP=17300; PGP=17301; LTP=17302; MGP=17303; MGD=17304
MONGOD=$HERE/../mongobin/mongod
mkdir -p "$W"; cd "$HERE"

[ -x "$B" ] || { echo "build DADABADA first: cargo build --release"; exit 1; }
[ -x "$GW" ] || (cd gateway && cargo build --release)

ready() { for _ in $(seq 1 300); do curl -s -o /dev/null -X POST "127.0.0.1:$1/v1/login" -d '{}' && return 0; sleep 0.1; done; return 1; }
cleanup() {
  [ -n "${PGW:-}" ] && kill "$PGW" 2>/dev/null || true
  [ -n "${LGW:-}" ] && kill "$LGW" 2>/dev/null || true
  [ -n "${MGW:-}" ] && kill "$MGW" 2>/dev/null || true
  [ -n "${MPID:-}" ] && kill "$MPID" 2>/dev/null && wait "$MPID" 2>/dev/null || true
  "$B" stop --data "$W/dd" >/dev/null 2>&1 || true
  python3 pg.py stop "$W/pgdata" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "== $NAME: $N records, in $W"
if [ "${QUERIES_ONLY:-}" = 1 ]; then
  "$B" start --data "$W/dd" >> "$W/dd.log" 2>&1 &
  ready $DDP
  python3 pg.py init "$W/pgdata" >/dev/null 2>&1 || true
else
rm -rf "$W/dd" "$W/pgdata" "$W/bench.sqlite"
"$B" init --port $DDP --bind 127.0.0.1 --data "$W/dd" >/dev/null
"$B" start --data "$W/dd" > "$W/dd.log" 2>&1 &
ready $DDP
echo loader-pass | "$B" admin --data "$W/dd" user add loader --level 99 >/dev/null
for l in 00 01 02 03; do echo "u$l-pass" | "$B" admin --data "$W/dd" user add u$l --level $l >/dev/null; done

python3 load_dd.py "$N" $DDP "$W/rows.tsv"
python3 ../status.py "$W/dd"
python3 pg.py init "$W/pgdata"
python3 load_sql.py "$W/rows.tsv" "$W/pgdata" "$W/bench.sqlite" records
fi
# MongoDB: its own process, a 2 GB cache (as PostgreSQL's shared buffers), reached over its local socket
rm -rf "$W/mongo"; mkdir -p "$W/mongo"
"$MONGOD" --dbpath "$W/mongo" --port $MGD --bind_ip 127.0.0.1 --unixSocketPrefix "$W" --wiredTigerCacheSizeGB 2 \
  --logpath "$W/mongo.log" --quiet & MPID=$!
MURI="mongodb://$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$W/mongodb-$MGD.sock")"
for _ in $(seq 1 200); do [ -S "$W/mongodb-$MGD.sock" ] && break; sleep 0.1; done
python3 load_mongo.py "$W/rows.tsv" "$MURI"

echo "-- settling"
"$B" stop --data "$W/dd" >/dev/null
"$B" start --data "$W/dd" >> "$W/dd.log" 2>&1 &
ready $DDP
python3 idle.py "$(pgrep -f -n "dadabada start --data $W/dd")"
python3 pg.py settle "$W/pgdata" records

"$GW" pg $PGP "$(python3 pg.py uri "$W/pgdata")" records > "$W/gw-pg.log" 2>&1 & PGW=$!
"$GW" sqlite $LTP "$W/bench.sqlite" records > "$W/gw-sqlite.log" 2>&1 & LGW=$!
"$GW" mongo $MGP "$MURI" records > "$W/gw-mongo.log" 2>&1 & MGW=$!
ready $PGP; ready $LTP; ready $MGP
cat "$W/gw-sqlite.log"

echo "-- queries"
python3 bench.py $DDP $PGP $LTP $MGP "results-$NAME.json"
