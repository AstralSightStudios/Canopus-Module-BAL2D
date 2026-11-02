#!/usr/bin/env python3
"""Frame timings of packs already on the Mirage 043 device: selects each one, lets it settle,
and averages a few stats readings (the module's own measurements, in guest time).

    scripts/perf.py [--wait S] [--samples N] NAME...     NAME: pack names as `link list` shows
    scripts/perf.py --all                               every installed pack

Each pack is sampled once it has played about --settle seconds (its frame count over its fps),
past its intro. Prints one line per pack (fps, update / draw / driver / GPU ms, triangles, calls, errors) and
writes the table to build/mirage/perf-<label>.txt when --label is given.
"""
import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIELDS = ["frames", "fps_x10", "update_ms", "draw_ms", "driver_ms", "gpu_ms", "triangles", "calls", "errors",
          "heap_free_kib", "pack", "flags", "ui_ms", "idle_ms", "late_ms", "flush_ms", "redraws"]


def link(*args, tries=3):
    for attempt in range(tries):
        r = subprocess.run([sys.executable, str(ROOT / "scripts/mirage_session.py"), "043", "link", *map(str, args)],
                           capture_output=True, text=True, timeout=600)
        if r.returncode == 0:
            return r.stdout
        time.sleep(3)
    raise RuntimeError(f"link {args}: {r.stderr.strip()[-300:]}")


def stats():
    out = link("stats")
    m = re.search(r"Stats \{(.*?)\}", out)
    if not m:
        return None
    vals = dict(re.findall(r"(\w+): (\d+)", m[1]))
    return {k: int(vals.get(k, 0)) for k in FIELDS}


def packs():
    found = {}
    for line in link("list").splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[0].startswith("0x"):
            found[parts[-1]] = parts[0]
    return found


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("names", nargs="*")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--wait", type=float, default=14)
    ap.add_argument("--settle", type=float, default=28,
                    help="lobby seconds (frames / fps) to let pass before sampling: past the longest "
                         "intro (23.7 s), so the idle loop is measured. In timing mode guest time runs "
                         "at a varying fraction of host time, so a host-time wait alone is not enough.")
    ap.add_argument("--samples", type=int, default=3)
    ap.add_argument("--every", type=float, default=3)
    ap.add_argument("--label", default="")
    a = ap.parse_args()
    ids = packs()
    names = sorted(n for n in ids if n.endswith("_home")) if a.all else a.names
    lines = [f"{'pack':14} {'fps':>5} {'upd':>5} {'draw':>5} {'drv':>5} {'flush':>5} {'gpu':>5} {'ui':>5} {'idle':>5} {'late':>5} {'tris':>6} {'calls':>5} {'err':>4} baked mode"]
    print(lines[0], flush=True)
    for name in names:
        if name not in ids:
            print(f"{name}: not installed", flush=True)
            continue
        link("select", ids[name])
        time.sleep(a.wait)
        # Frames count from boot: the lobby's own time is the frames since it first showed up.
        deadline, first = time.time() + 900, None
        while time.time() < deadline:
            st = stats()
            if st and st["pack"] == int(ids[name], 16) and st["fps_x10"] > 0:
                first = st["frames"] if first is None else first
                if (st["frames"] - first) * 10 / st["fps_x10"] >= a.settle:
                    break
            time.sleep(5)
        got = []
        for _ in range(a.samples):
            st = stats()
            if st and st["pack"] == int(ids[name], 16) and st["fps_x10"] > 0:
                got.append(st)
            time.sleep(a.every)
        if not got:
            print(f"{name}: no frames", flush=True)
            continue
        avg = {k: sum(g[k] for g in got) / len(got) for k in FIELDS}
        line = (f"{name:14} {avg['fps_x10'] / 10:5.1f} {avg['update_ms']:5.1f} {avg['draw_ms']:5.1f} "
                f"{avg['driver_ms']:5.1f} {avg['flush_ms']:5.1f} {avg['gpu_ms']:5.1f} {avg['ui_ms']:5.1f} {avg['idle_ms']:5.1f} {avg['late_ms']:5.1f} "
                f"{avg['triangles']:6.0f} {avg['calls']:5.0f} {got[-1]['errors']:4d} {'yes' if got[-1]['flags'] & 16 else 'no':5} {'direct' if got[-1]['flags'] & 32 else 'own'}")
        lines.append(line)
        print(line, flush=True)
    if a.label:
        out = ROOT / f"build/mirage/perf-{a.label}.txt"
        out.write_text("\n".join(lines) + "\n")
        print(f"written {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
