"""Waits until a process has been idle (under 10% of one CPU; an idle DADABADA uses about 3% for its status ticker, its compaction far more) for 20 seconds in a row. Usage: python3 idle.py <pid>"""
import os, sys, time

pid, tick = int(sys.argv[1]), os.sysconf("SC_CLK_TCK")


def cpu():
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / tick


quiet, t0 = 0, time.time()
prev = cpu()
while quiet < 4:
    time.sleep(5)
    now = cpu()
    quiet = quiet + 1 if (now - prev) / 5 < 0.10 else 0
    prev = now
print(f"idle after {time.time() - t0:.0f} s", flush=True)
