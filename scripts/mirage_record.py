#!/usr/bin/env python3
"""Screen recordings of PetEngine on a Mirage device.

    mirage_record.py DEVICE OUT.mp4 STEP... [--fps-scale 2]

Steps are mirage_pet_test.py's (tap, long, stroke, tapat:X,Y, swipe:..., hold:..., wait:MS, back,
shot:NAME) plus `link:ARGS` (a PetLink command through the AstroBox bridge, as the phone plugin
would send it: `link:eval pet.say('hi')`, `link:activate hello`).

Mirage has no video capture, so screenshots are taken back to back (about 8 per second) and ffmpeg
stitches them with their real timing; frames stay next to the video in OUT_frames/. One capture
at a time: parallel requests come back out of order. Emulator only: the frame rate is the
capture's, not the watch's.
"""
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import mirage_pet_test as pt  # noqa: E402

MIRAGECTL = pt.MIRAGECTL
KEYS = {"pet-dev-179": "179", "pet-dev-043": "043"}


class Recorder:
    def __init__(self, device, frames):
        self.device, self.frames = device, Path(frames)
        self.frames.mkdir(parents=True, exist_ok=True)
        for old in self.frames.glob("*.png"):
            old.unlink()
        self.shots, self.stop_flag = [], threading.Event()

    def _work(self):
        n = 0
        while not self.stop_flag.is_set():
            path = self.frames / f".f{n:05d}.png"
            begin = time.monotonic()
            subprocess.run([str(MIRAGECTL), "screenshot", self.device, str(path)], capture_output=True)
            end = time.monotonic()
            self.shots.append(((begin + end) / 2, path))
            n += 1

    def __enter__(self):
        self.t0 = time.monotonic()
        self.thread = threading.Thread(target=self._work)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop_flag.set()
        self.thread.join()

    def encode(self, out, scale=2):
        shots = sorted(s for s in self.shots if s[1].exists() and s[1].stat().st_size > 0)
        listing = self.frames / "frames.txt"
        lines = []
        named = None
        for (t, path), following in zip(shots, shots[1:] + [None]):
            named = self.frames / f"t{int((t - self.t0) * 1000):06d}.png"
            path.rename(named)
            duration = (following[0] - t) if following else 0.5
            lines += [f"file '{named.name}'", f"duration {duration:.3f}"]
        lines.append(f"file '{named.name}'")
        listing.write_text("\n".join(lines) + "\n")
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", str(listing),
                        "-vf", f"scale=iw*{scale}:ih*{scale}:flags=lanczos,fps=30,format=yuv420p",
                        "-c:v", "libx264", "-crf", "18", "-movflags", "+faststart", str(out)], check=True)
        seconds = shots[-1][0] - shots[0][0] if shots else 0
        print(f"{out}: {len(shots)} frames over {seconds:.1f} s ({len(shots) / max(seconds, 0.1):.1f} fps)")


def link(device, args):
    subprocess.run([sys.executable, str(HERE / "mirage_session.py"), KEYS[device], "link", *args.split(" ", 1)[0:1],
                    *([args.split(" ", 1)[1]] if " " in args else [])], capture_output=True, text=True)


def main():
    args = sys.argv[1:]
    scale = 2
    if "--fps-scale" in args:
        i = args.index("--fps-scale")
        scale = int(args[i + 1])
        del args[i:i + 2]
    device, out, steps = args[0], Path(args[1]), args[2:]
    shots = out.with_name(out.stem + "_shots")
    shots.mkdir(parents=True, exist_ok=True)
    pt.mt.wake(device)
    # Without the pet on screen every touch below would land in the system's own apps.
    if pt.find_pet(device) is None:
        sys.exit("no pet on screen: is the module running (and the Supervisor started)?")
    with Recorder(device, out.with_name(out.stem + "_frames")) as rec:
        for step in steps:
            print("step", step, flush=True)
            if step.startswith("link:"):
                link(device, step[5:])
            else:
                pt.run_step(device, step, shots)
    rec.encode(out, scale)


if __name__ == "__main__":
    main()
