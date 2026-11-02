#!/usr/bin/env python3
"""Drives a Mirage device quickly (the panel sleeps after ~5 s of no input) and saves screenshots.

    shot.py DEVICE OUT.png [--tap X Y] [--wait S] [--keep S [X Y]] [--shot NAME]...

--keep S pokes an inert point (default top centre) every 2 s for S seconds so the screen stays
on; --shot saves an extra screenshot next to OUT. The final screenshot goes to OUT.
"""
import subprocess, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mirage_dev import Dev, MIRAGECTL

dev, out = sys.argv[1], Path(sys.argv[2])
d = Dev(dev, out.parent)
w, h = d.size

def ctl(*a):
    subprocess.run([str(MIRAGECTL), a[0], dev, *map(str, a[1:])], capture_output=True, timeout=60)

d.wake()
args = sys.argv[3:]
while args:
    a = args.pop(0)
    if a == "--tap":
        ctl("tap", args.pop(0), args.pop(0)); time.sleep(0.3)
    elif a == "--wait":
        time.sleep(float(args.pop(0)))
    elif a == "--keep":
        secs = float(args.pop(0))
        x, y = (w // 2, 12)
        if args and not args[0].startswith("--"):
            x, y = int(args.pop(0)), int(args.pop(0))
        end = time.time() + secs
        while time.time() < end:
            ctl("tap", x, y)
            time.sleep(min(2.0, max(0.0, end - time.time())))
    elif a == "--shot":
        p = out.parent / args.pop(0)
        ctl("screenshot", p); print(p)
ctl("screenshot", out)
print(out)
