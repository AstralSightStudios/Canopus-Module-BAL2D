#!/usr/bin/env python3
"""Arbitrary touch input for a running Mirage device through its VNC socket (RFB pointer
events): back-and-forth strokes, timed holds and paths that `miragectl tap/swipe` cannot do.

    mirage_touch.py DEVICE tap X Y
    mirage_touch.py DEVICE hold X Y MS
    mirage_touch.py DEVICE stroke X Y [AMPLITUDE] [COUNT]     rub left-right over (X, Y)
    mirage_touch.py DEVICE drag X0 Y0 X1 Y1 [MS]
    mirage_touch.py DEVICE log [LINES]                         tail of the firmware console log
"""
import glob
import os
import socket
import struct
import subprocess
import sys
import time

MIRAGECTL = os.environ.get("MIRAGECTL", "/Volumes/EXT0/Mirage/.build/out/Products/Debug/miragectl")


def device_id(name):
    out = subprocess.run([MIRAGECTL, "list"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) > 1 and parts[1] == name:
            return parts[0]
    sys.exit(f"no device {name}")


def vnc_socket(name):
    short = device_id(name).lower()
    hits = glob.glob(f"/var/folders/*/*/T/mirage-{short}/vnc.sock") + glob.glob(f"/tmp/mirage-{short}/vnc.sock")
    if not hits:
        sys.exit(f"no VNC socket for {name} (is it booted?)")
    return hits[0]


def console_log(name):
    info = subprocess.run([MIRAGECTL, "info", name], capture_output=True, text=True).stdout
    full = [l.split()[-1] for l in info.splitlines() if l.startswith("id:")][0]
    hits = glob.glob(f"/Volumes/EXT0/Mirage/.scratch/Library/Devices/{full}/Logs/console.log")
    return hits[0] if hits else None


class Pointer:
    def __init__(self, name):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(vnc_socket(name))
        self.s.recv(12)
        self.s.sendall(b"RFB 003.008\n")
        n = self.recv(1)[0]
        types = self.recv(n)
        self.s.sendall(bytes([1 if 1 in types else types[0]]))
        if self.recv(4) != b"\0\0\0\0":
            sys.exit("VNC security failed")
        self.s.sendall(b"\x01")
        head = self.recv(24)
        name_len = struct.unpack(">I", head[20:24])[0]
        self.recv(name_len)
        # SetEncodings makes QEMU re-check the pointer type: without it a fresh client's pointer
        # is relative and absolute touch positions are ignored.
        self.s.sendall(struct.pack(">BBHi", 2, 0, 1, 0))

    def recv(self, n):
        data = b""
        while len(data) < n:
            chunk = self.s.recv(n - len(data))
            if not chunk:
                raise IOError("VNC closed")
            data += chunk
        return data

    def event(self, x, y, down):
        self.s.sendall(struct.pack(">BBHH", 5, 1 if down else 0, int(x), int(y)))

    def path(self, points, step_ms=30, hold_first_ms=0):
        x, y = points[0]
        self.event(x, y, True)
        time.sleep(max(step_ms, hold_first_ms) / 1000)
        for x, y in points[1:]:
            self.event(x, y, True)
            time.sleep(step_ms / 1000)
        self.event(x, y, False)
        time.sleep(0.05)


def stroke_points(x, y, amplitude=14, count=6):
    pts = [(x, y)]
    for i in range(count):
        side = 1 if i % 2 == 0 else -1
        for k in range(1, 5):
            pts.append((x + side * amplitude * k / 4, y))
        for k in range(3, -1, -1):
            pts.append((x + side * amplitude * k / 4, y))
    return pts


def wake(name):
    """Mirage turns the panel off a few seconds after the last touch; a touch on a dark panel
    only wakes it. Tap an inert spot until the screenshot is lit."""
    probe = "/tmp/mirage-touch-probe.png"
    for _ in range(4):
        subprocess.run([MIRAGECTL, "screenshot", name, probe], capture_output=True)
        if os.path.getsize(probe) > 3000:
            return
        data = open(probe, "rb").read(24)
        wide = struct.unpack(">I", data[16:20])[0] > 300
        # Band 10 Pro wakes on a quick double tap; Band 11 on a single one.
        for _ in range(2 if wide else 1):
            subprocess.run([MIRAGECTL, "tap", name, "168" if wide else "106", "20"], capture_output=True)
        time.sleep(1.2)


def main():
    name, cmd, *rest = sys.argv[1:]
    if cmd != "log" and "--no-wake" not in rest:
        wake(name)
    rest = [r for r in rest if r != "--no-wake"]
    if cmd == "log":
        path = console_log(name)
        lines = open(path, errors="replace").read().splitlines()[-int(rest[0] if rest else 40):]
        print("\n".join(lines))
        return
    nums = [float(v) for v in rest]
    p = Pointer(name)
    if cmd == "tap":
        p.path([(nums[0], nums[1])], step_ms=90)
    elif cmd == "hold":
        p.path([(nums[0], nums[1]), (nums[0], nums[1])], step_ms=10, hold_first_ms=nums[2])
    elif cmd == "stroke":
        amp = nums[2] if len(nums) > 2 else 14
        count = int(nums[3]) if len(nums) > 3 else 6
        p.path(stroke_points(nums[0], nums[1], amp, count), step_ms=25)
    elif cmd == "drag":
        ms = nums[4] if len(nums) > 4 else 500
        steps = 16
        pts = [(nums[0] + (nums[2] - nums[0]) * i / steps, nums[1] + (nums[3] - nums[1]) * i / steps) for i in range(steps + 1)]
        p.path(pts, step_ms=ms / steps, hold_first_ms=80)
    else:
        sys.exit("unknown command")


if __name__ == "__main__":
    main()
