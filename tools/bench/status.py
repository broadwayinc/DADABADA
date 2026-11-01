"""Prints a server's catalog record count and its background queue (outbox) length, from the admin socket.
Usage: python3 status.py <data folder>"""
import json, socket, sys

s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1] + "/admin.sock")
s.sendall(b'{"cmd":"status"}\n')
d = b""
while not d.endswith(b"\n"):
    d += s.recv(65536)
v = json.loads(d)["ok"]
print(v["records"], v["outbox"])
