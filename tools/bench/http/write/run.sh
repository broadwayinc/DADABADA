#!/usr/bin/env bash
# Write benchmark, start to finish. Usage: ./run.sh <records> <work dir>   (writes results.json here)
set -euo pipefail
N=$1; W=$(realpath -m "$2")/write
HERE=$(cd "$(dirname "$0")" && pwd)
B=$HERE/../../../../target/release/dadabada
GW=$HERE/../gateway/target/release/sqlgw
DDP=17500; PGP=17501; LTP=17502
mkdir -p "$W"; cd "$HERE"
ready() { for _ in $(seq 1 300); do curl -s -o /dev/null -X POST "127.0.0.1:$1/v1/login" -d '{}' && return 0; sleep 0.1; done; return 1; }
cleanup() {
  [ -n "${PGW:-}" ] && kill "$PGW" 2>/dev/null || true
  [ -n "${LGW:-}" ] && kill "$LGW" 2>/dev/null || true
  "$B" stop --data "$W/dd" >/dev/null 2>&1 || true
  python3 ../pg.py stop "$W/pgdata" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "== write: $N records, in $W"
rm -rf "$W/dd" "$W/pgdata" "$W/bench.sqlite"*
"$B" init --port $DDP --bind 127.0.0.1 --data "$W/dd" >/dev/null
"$B" start --data "$W/dd" > "$W/dd.log" 2>&1 &
ready $DDP
echo loader-pass | "$B" admin --data "$W/dd" user add loader --level 99 >/dev/null
DDPID=$(pgrep -f -n "dadabada start --data $W/dd")

python3 ../pg.py init "$W/pgdata"
python3 schema.py "$W/pgdata" "$W/bench.sqlite"
URI=$(python3 ../pg.py uri "$W/pgdata")
"$GW" pg $PGP "$URI" records write > "$W/gw-pg.log" 2>&1 & PGW=$!
"$GW" sqlite $LTP "$W/bench.sqlite" records write > "$W/gw-sqlite.log" 2>&1 & LGW=$!
ready $PGP; ready $LTP

python3 bench.py "$N" $DDP $PGP $LTP "$DDPID" "$W/pgdata" results.json
echo "-- counts"
python3 ../../status.py "$W/dd"
python3 -c "
import sys, sqlite3; sys.path.insert(0, '../../pylib'); import pgserver, psycopg
print('PostgreSQL', psycopg.connect(pgserver.get_server('$W/pgdata', cleanup_mode=None).get_uri()).execute('select count(*) from records').fetchone()[0])
print('SQLite', sqlite3.connect('$W/bench.sqlite').execute('select count(*) from records').fetchone()[0])"
echo "DADABADA data folder: $(du -sh "$W/dd" | cut -f1)  PostgreSQL: $(du -sh "$W/pgdata" | cut -f1)  SQLite: $(du -ch "$W"/bench.sqlite* | tail -1 | cut -f1)"
