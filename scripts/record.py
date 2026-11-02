#!/usr/bin/env python3
"""Records a Mirage device: back-to-back screenshots stitched with their real timing.

    record.py DEVICE OUT.mp4 SECONDS [--tap AT X Y]...

Taps are sent AT seconds after the start (each in its own thread so capture continues).
Emulator only: the frame rate is the capture's (a few per second), not the watch's.
"""
import subprocess, sys, threading, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mirage_dev import MIRAGECTL

dev, out, seconds = sys.argv[1], Path(sys.argv[2]), float(sys.argv[3])
taps = []
args = sys.argv[4:]
while args:
    if args[0] == "--tap":
        taps.append((float(args[1]), args[2], args[3])); args = args[4:]
    elif args[0] == "--swipe":
        taps.append((float(args[1]), args[2:7], None)); args = args[7:]
    else:
        args = args[1:]
frames = out.with_name(out.stem + "_frames")
frames.mkdir(parents=True, exist_ok=True)
for f in frames.glob("*.png"):
    f.unlink()

def fire(at, a, b):
    time.sleep(at)
    if b is None:
        subprocess.run([str(MIRAGECTL), "swipe", dev, *a], capture_output=True)
    else:
        subprocess.run([str(MIRAGECTL), "tap", dev, a, b], capture_output=True)

for t in taps:
    threading.Thread(target=fire, args=t, daemon=True).start()
start = time.time()
stamps = []
i = 0
while time.time() - start < seconds:
    t = time.time() - start
    p = frames / f"{i:05d}.png"
    subprocess.run([str(MIRAGECTL), "screenshot", dev, p], capture_output=True)
    if p.exists():
        stamps.append((p, t)); i += 1
# Concat demuxer with real durations.
lst = frames / "list.txt"
with open(lst, "w") as f:
    for k, (p, t) in enumerate(stamps):
        d = (stamps[k + 1][1] - t) if k + 1 < len(stamps) else 0.2
        f.write(f"file '{p.name}'\nduration {d:.3f}\n")
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", str(lst), "-vsync", "vfr",
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", str(out)])
print(f"{out}: {len(stamps)} frames over {seconds:.0f} s ({len(stamps)/seconds:.1f} captures/s)")
