#!/usr/bin/env python3
"""Bluetooth under load on the Mirage 043 device: whether the phone side still connects and
transfers while the module draws.

    scripts/btstress.py [--rounds N]

1. Lua face, a light lobby (Shiroko): N link calls, each a fresh connection.
2. The same face: CH0335 deleted and installed again (a transfer while drawing).
3. The animated stock face with the full-screen layer and the heaviest lobby (Hoshino): N link
   calls. Then the layer is switched off again.

Every call's time and outcome is printed; the summary counts failures.
"""
import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import mirage_session as ms  # noqa: E402

LUA_FACE = "000000000B2D"
STOCK_FACE = "120917385439"
PACKS = ROOT / "review/packs/v7"


def link(*args):
    t0 = time.time()
    r = subprocess.run([sys.executable, str(ROOT / "scripts/mirage_session.py"), "043", "link", *map(str, args)],
                       capture_output=True, text=True, timeout=900)
    out = (r.stdout.strip().splitlines() or [""])[-1]
    return r.returncode == 0 and "panicked" not in r.stderr, time.time() - t0, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=5)
    a = ap.parse_args()
    s = ms.Session("043", str(ROOT / "review/mirage/btstress"))
    fails = 0

    def calls(label):
        nonlocal fails
        for i in range(a.rounds):
            ok, dt, out = link("stats")
            fails += not ok
            print(f"{label} {i + 1}: {'ok' if ok else 'FAIL'} {dt:5.1f} s {out[:90]}", flush=True)

    s.phone('setWatchface={"id":"%s"}' % LUA_FACE)
    ids = {}
    out = subprocess.run([sys.executable, str(ROOT / "scripts/mirage_session.py"), "043", "link", "list"],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[0].startswith("0x"):
            ids[parts[-1]] = parts[0]
    link("select", ids["Shiroko_home"])
    time.sleep(10)
    calls("light")
    if "CH0335_home" in ids:
        link("delete", ids["CH0335_home"])
    ok, dt, out = link("install", PACKS / "CH0335_home.bal2d")
    fails += not ok
    print(f"transfer: {'ok' if ok else 'FAIL'} {dt:5.1f} s {out[:90]}", flush=True)
    link("select", ids["Hoshino_home"])
    s.phone('setWatchface={"id":"%s"}' % STOCK_FACE)
    ok, _, _ = link("options", 1)
    time.sleep(15)
    calls("heavy overlay")
    for _ in range(5):
        ok, _, _ = link("options", 0)
        if ok:
            break
    s.phone('setWatchface={"id":"%s"}' % LUA_FACE)
    print(f"{fails} failed", flush=True)


if __name__ == "__main__":
    main()
