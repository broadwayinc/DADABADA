"""PostgreSQL helper for the HTTP run, using the private PostgreSQL from ../pylib (pgserver).
  python3 pg.py init <pgdata>     create the cluster, write the bench settings, restart so they apply
  python3 pg.py uri <pgdata>      print the connection URI (local socket)
  python3 pg.py settle <pgdata> <table>   CHECKPOINT and VACUUM ANALYZE, then print the settings in force
  python3 pg.py stop <pgdata>
"""
import os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "pylib"))
import pgserver, psycopg  # noqa: E402

PG_CTL = os.path.join(HERE, "..", "pylib", "pgserver", "pginstall", "bin", "pg_ctl")
cmd, pgdata = sys.argv[1], os.path.abspath(sys.argv[2])

if cmd == "init":
    pgserver.get_server(pgdata, cleanup_mode=None)
    conf = os.path.join(pgdata, "postgresql.conf")
    if "# bench settings" not in open(conf).read():
        with open(conf, "a") as f:
            # generous to PostgreSQL: a buffer cache that holds the whole data set, no JIT for short queries,
            # and a plan made for each query's actual values
            f.write("\n# bench settings\nshared_buffers = 2GB\njit = off\nplan_cache_mode = force_custom_plan\n")
    subprocess.run([PG_CTL, "-D", pgdata, "-w", "restart", "-l", os.path.join(pgdata, "log.txt")], check=True,
                   stdout=subprocess.DEVNULL)
elif cmd == "uri":
    print(pgserver.get_server(pgdata, cleanup_mode=None).get_uri())
elif cmd == "settle":
    with psycopg.connect(pgserver.get_server(pgdata, cleanup_mode=None).get_uri(), autocommit=True) as c:
        c.execute("CHECKPOINT")
        c.execute(f"VACUUM ANALYZE {sys.argv[3]}")
        for s in ["server_version", "shared_buffers", "jit", "plan_cache_mode"]:
            print(s, "=", c.execute(f"SHOW {s}").fetchone()[0])
elif cmd == "stop":
    subprocess.run([PG_CTL, "-D", pgdata, "-w", "stop", "-m", "fast"], check=True, stdout=subprocess.DEVNULL)
