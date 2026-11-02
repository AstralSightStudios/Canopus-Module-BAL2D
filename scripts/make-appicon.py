#!/usr/bin/env python3
"""Draws the launcher icon (original artwork, no game assets): assets/appicon.bin, a 117x117
LVGL v9 ARGB8888 image (12-byte header, straight alpha, B G R A bytes), a PNG preview, and the
AstroBox plugin's 256x256 icon.png.

    scripts/make-appicon.py
"""
import math, struct, zlib
from pathlib import Path

N, SS = 117, 4
ROOT = Path(__file__).resolve().parent.parent


def sample(x, y):
    """Straight RGBA (0..1) at icon coordinates (0..N)."""
    cx = cy = N / 2
    dx, dy = x - cx, y - cy
    r = math.hypot(dx, dy)
    if r > N / 2 - 0.5:
        return (0, 0, 0, 0)
    t = (y / N)
    # Sky gradient: deep blue at the bottom to light cyan at the top.
    top, bottom = (0.55, 0.85, 1.0), (0.12, 0.33, 0.78)
    col = [top[i] * (1 - t) + bottom[i] * t for i in range(3)]
    # Soft vignette.
    v = 1 - 0.25 * (r / (N / 2)) ** 2
    col = [c * v for c in col]
    # Halo: a tilted ring above the centre.
    hx, hy = cx, cy - 14
    ex, ey = dx, (y - hy) * 2.6
    ring = abs(math.hypot(ex, ey) - 30)
    if ring < 5.5:
        a = min(1, (5.5 - ring) / 1.5)
        glow = (1.0, 0.97, 0.85)
        col = [col[i] * (1 - a) + glow[i] * a for i in range(3)]
    # Four-point sparkle under the halo.
    sx, sy = x - cx, y - (cy + 18)
    star = abs(sx) * abs(sy)
    if abs(sx) < 16 and abs(sy) < 16 and star < 9 + 0 * r:
        a = max(0, 1 - (abs(sx) + abs(sy)) / 18)
        col = [col[i] * (1 - a) + 1.0 * a for i in range(3)]
    return (*col, 1.0)


def render(size):
    k = N / size
    px = []
    for j in range(size):
        for i in range(size):
            acc = [0.0] * 4
            for sj in range(SS):
                for si in range(SS):
                    c = sample((i + (si + 0.5) / SS) * k, (j + (sj + 0.5) / SS) * k)
                    a = c[3]
                    acc[0] += c[0] * a
                    acc[1] += c[1] * a
                    acc[2] += c[2] * a
                    acc[3] += a
            a = acc[3] / (SS * SS)
            rgb = [acc[k] / acc[3] if acc[3] else 0 for k in range(3)]
            px.append([round(rgb[0] * 255), round(rgb[1] * 255), round(rgb[2] * 255), round(a * 255)])
    return px


def png(path, px, n):
    raw = b"".join(b"\0" + b"".join(bytes(p) for p in px[r * n:(r + 1) * n]) for r in range(n))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    px = render(N)
    head = struct.pack("<BBHHHHH", 0x19, 0x10, 0, N, N, N * 4, 0)
    body = b"".join(bytes([p[2], p[1], p[0], p[3]]) for p in px)
    out = ROOT / "assets/appicon.bin"
    out.write_bytes(head + body)
    png(ROOT / "assets/appicon.png", px, N)
    png(ROOT / "plugins/astrobox-bal2d-import/icon.png", render(256), 256)
    print(out, len(head + body), "bytes")


main()
