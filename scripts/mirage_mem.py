#!/usr/bin/env python3
"""Reads emulated memory of a running Mirage device over its auxiliary QMP socket.

    mirage_mem.py DEVICE xp ADDR [COUNT]      dump COUNT words
    mirage_mem.py DEVICE hmp "COMMAND"        any HMP command
"""
import glob, json, socket, subprocess, sys, os
sys.path.insert(0, os.path.dirname(__file__))
from mirage_touch import device_id

def qmp(name):
    short = device_id(name).lower()
    path = glob.glob(f"/var/folders/*/*/T/mirage-{short}/qmp-aux.sock")[0]
    s = socket.socket(socket.AF_UNIX)
    s.connect(path)
    f = s.makefile("rw")
    f.readline()
    f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
    def hmp(cmd):
        f.write(json.dumps({"execute": "human-monitor-command", "arguments": {"command-line": cmd}}) + "\n"); f.flush()
        while True:
            line = json.loads(f.readline())
            if "return" in line or "error" in line:
                return line.get("return", line.get("error"))
    return hmp

def words(hmp, addr, count=1):
    out = hmp(f"xp /{count}wx {addr:#x}")
    vals = []
    for line in out.splitlines():
        if ":" in line:
            vals += [int(v, 16) for v in line.split(":", 1)[1].split()]
    return vals

if __name__ == "__main__":
    name, cmd, *rest = sys.argv[1:]
    hmp = qmp(name)
    if cmd == "xp":
        print(hmp(f"xp /{rest[1] if len(rest) > 1 else 1}wx {rest[0]}"))
    else:
        print(hmp(rest[0]))
