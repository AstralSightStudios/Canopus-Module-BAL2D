#!/usr/bin/env python3
"""Draws BAL2D's icons around the halo of Blue Archive's title logo, in pure Python:
assets/appicon.bin, the 117x117 launcher icon (LVGL v9 ARGB8888, 12-byte header, straight alpha,
B G R A bytes); assets/appicon.png, its preview (gitignored); and the AstroBox plugin's 256x256
icon.png.

The halo (ring Fill-19, blades Fill-21 and Fill-23) is copied path for path from the Japanese
title logo, https://commons.wikimedia.org/wiki/File:Blue_Archive_JP_logo.svg (Commons: public
domain, trademarked; taken from Yostar's site). Framing follows LyraPlayer's icons: the launcher
circle leaves a 3 px transparent margin (alpha 143 at x=3 on the centre row), the plugin icon is a
full-bleed circle with a sync badge in a frosted cap at the bottom.

    scripts/make-appicon.py
"""
import math, re, struct, zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Logo units (the SVG's viewBox). The blade Fill-21 sits in a translate(181.3363, 0) group.
RING = (0.0, "M327.076,114.61c-3.241-3.397-6.308-6.945-9.174-10.647c-6.095-7.965-11.258-16.735-14.084-26.089"
        "c-1.435-4.648-2.219-9.459-1.96-14.128c0-1.175,0.223-2.314,0.346-3.465c0.265-1.117,0.418-2.268,0.816-3.341"
        "c0.624-2.2,1.658-4.245,2.874-6.151c1.162-1.951,2.751-3.61,4.403-5.179c1.706-1.529,3.634-2.83,5.666-3.978"
        "c4.117-2.233,8.738-3.677,13.485-4.576c9.536-1.796,19.573-1.496,29.399-0.09c19.724,2.895,38.871,10.049,56.478,19.743"
        "c17.559,9.777,33.852,22.206,46.744,37.606c6.361,7.712,11.885,16.255,15.082,25.63c0.993,2.92,1.75,5.925,2.14,8.954"
        "h3.331c-0.357-3.429-1.137-6.796-2.184-10.04c-3.257-10.008-8.887-18.946-15.338-27.015"
        "c-13.075-16.091-29.566-29.038-47.407-39.326c-17.866-10.265-37.365-18.018-57.956-21.443"
        "c-10.29-1.56-20.913-2.107-31.454-0.162c-5.24,1.036-10.46,2.684-15.29,5.351c-2.392,1.371-4.696,2.951-6.772,4.843"
        "c-2.025,1.932-3.963,4.026-5.412,6.467c-1.461,2.413-2.666,5.01-3.372,7.734c-0.442,1.34-0.613,2.733-0.885,4.103"
        "c-0.125,1.381-0.334,2.764-0.308,4.142c-0.133,5.535,1.067,10.884,2.828,15.89c3.628,10.029,9.619,18.739,16.335,26.555"
        "c2.6,3,5.333,5.867,8.174,8.613")
BLADES = [
    (181.3363, "M267.565,0c-10.904,22.511-22.64,44.53-34.67,66.379c-5.936,10.97-12.174,21.762-18.271,32.638"
     "l-18.741,32.363l-19.195,32.093c-6.559,10.604-12.959,21.302-19.676,31.812c-13.265,21.12-26.831,42.061-41.237,62.512"
     "c10.894-22.517,22.626-44.54,34.658-66.387c5.931-10.973,12.182-21.759,18.271-32.638l18.752-32.355l19.206-32.088"
     "c6.554-10.606,12.964-21.297,19.676-31.811C239.61,41.402,253.172,20.456,267.565,0"),
    (0.0, "M494.929,163.919c-18.256-9.359-36.19-19.254-54.012-29.338c-8.941-4.992-17.767-10.178-26.646-15.274"
     "l-26.464-15.575l-26.291-15.87c-8.702-5.394-17.464-10.684-26.106-16.18c-17.346-10.884-34.577-21.96-51.485-33.579"
     "c18.259,9.352,36.195,19.245,54.017,29.332c8.943,4.989,17.764,10.183,26.646,15.274l26.461,15.583l26.286,15.877"
     "c8.704,5.391,17.463,10.689,26.104,16.181C460.785,141.235,478.017,152.309,494.929,163.919"),
]
CENTRE, REACH = (367.8, 125.8), 149.7  # smallest circle around the whole halo
CROSS = (389.4, 101.0)                 # where the blades cross
BOLD = 2.3                             # dilation: the logo's 3.3-unit ring would be a 1.3 px hairline


def rgb(h):
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


INK, BLUE = rgb("2b2b2b"), rgb("128afa")  # the logo's two colours
WHITE = (1.0, 1.0, 1.0)
SKY = [(0.0, rgb("74cdff")), (0.55, rgb("3fb1f8")), (1.0, rgb("1a87e8"))]  # top to bottom
RIM, NAVY = rgb("1f86e0"), rgb("1d4f8c")
CAP = [(0.0, WHITE), (0.5, rgb("e4f5ff")), (1.0, rgb("7fcdfb"))]  # frosted cap, top to bottom

# Size, circle radius (LyraPlayer's plugin icon has alpha 197 at x=0), halo diameter / circle
# diameter, halo lift (circle radii), sync badge.
APP = (117, 117 / 2 - 3.44, 1.08, 0.0, False)
PLUGIN = (256, 256 / 2 - 0.23, 0.84, 0.17, True)


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def ramp(stops, t):
    t = max(0.0, min(1.0, t))
    for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
        if t <= t1:
            return mix(c0, c1, (t - t0) / (t1 - t0))
    return stops[-1][1]


def flatten(d, dx, steps=24):
    """SVG path data (M L H V C, either case; enough for these paths) -> closed polygons."""
    toks = re.findall(r"[MmLlHhVvCc]|-?(?:\d+\.?\d*|\.\d+)", d)
    polys, cur, x, y, i, cmd = [], [], 0.0, 0.0, 0, None
    while i < len(toks):
        if toks[i].isalpha():
            cmd, i = toks[i], i + 1
        ox, oy = (x, y) if cmd.islower() else (0.0, 0.0)
        c = cmd.upper()
        if c == "C":
            p = [float(v) for v in toks[i:i + 6]]
            i += 6
            x1, y1, x2, y2, x3, y3 = ox + p[0], oy + p[1], ox + p[2], oy + p[3], ox + p[4], oy + p[5]
            for k in range(1, steps + 1):
                t, u = k / steps, 1 - k / steps
                cur.append((u**3 * x + 3 * u * u * t * x1 + 3 * u * t * t * x2 + t**3 * x3,
                            u**3 * y + 3 * u * u * t * y1 + 3 * u * t * t * y2 + t**3 * y3))
            x, y = x3, y3
            continue
        if c in "ML":
            x, y, i = ox + float(toks[i]), oy + float(toks[i + 1]), i + 2
        elif c == "H":
            x, i = ox + float(toks[i]), i + 1
        else:
            y, i = oy + float(toks[i]), i + 1
        if c == "M":
            if cur:
                polys.append(cur)
            cur, cmd = [], "l" if cmd == "m" else "L"
        cur.append((x, y))
    polys.append(cur)
    return [[(px + dx, py) for px, py in p] for p in polys]


def coverage(polys, n, sub=16):
    """Even-odd area coverage (0..1) on an n x n grid: exact along x, `sub` scanlines per row."""
    edges = [(x0, y0, x1, y1) if y0 < y1 else (x1, y1, x0, y0)
             for p in polys for (x0, y0), (x1, y1) in zip(p, p[1:] + p[:1]) if y0 != y1]
    cov, w = [0.0] * (n * n), 1.0 / sub
    for row in range(n):
        live = [e for e in edges if e[1] < row + 1 and e[3] > row]
        for s in range(sub if live else 0):
            ys = row + (s + 0.5) * w
            xs = sorted(x0 + (ys - y0) * (x1 - x0) / (y1 - y0) for x0, y0, x1, y1 in live if y0 <= ys < y1)
            for a, b in zip(xs[0::2], xs[1::2]):
                a, b = max(a, 0.0), min(b, float(n))
                ia, ib = int(a), int(b)
                if b <= a:
                    continue
                if ia == ib:
                    cov[row * n + ia] += (b - a) * w
                    continue
                cov[row * n + ia] += (ia + 1 - a) * w
                for k in range(ia + 1, min(ib, n)):
                    cov[row * n + k] += w
                if ib < n:
                    cov[row * n + ib] += (b - ib) * w
    return [min(1.0, v) for v in cov]


def union(a, b):
    return [1 - (1 - p) * (1 - q) for p, q in zip(a, b)]


def shape(path, n, place, bold):
    """Coverage of one logo path, grown by `bold` px (the union of nine shifted copies)."""
    polys = [[place(x, y) for x, y in p] for p in flatten(path[1], path[0])]
    acc = None
    for k in range(9):
        ox, oy = (0, 0) if k == 0 else (bold * math.cos(k * math.pi / 4), bold * math.sin(k * math.pi / 4))
        cv = coverage([[(x + ox, y + oy) for x, y in p] for p in polys], n)
        acc = cv if acc is None else [max(p, q) for p, q in zip(acc, cv)]
    return acc


def blur(a, n, r):
    for _ in range(3):
        for horizontal in (True, False):
            out = [0.0] * (n * n)
            for line in range(n):
                idx = [line * n + k for k in range(n)] if horizontal else [k * n + line for k in range(n)]
                pre = [0.0]
                for i in idx:
                    pre.append(pre[-1] + a[i])
                for k, i in enumerate(idx):
                    out[i] = (pre[min(n, k + r + 1)] - pre[max(0, k - r)]) / (2 * r + 1)
            a = out
    return a


def sync_arrows(R):
    """Two arcs with chevron heads around (0, 0), in px: the badge's strokes."""
    ra, head, lines = 0.155 * R, 0.075 * R, []
    for a0, a1 in ((200, 338), (20, 158)):
        arc = [(ra * math.cos(math.radians(a)), ra * math.sin(math.radians(a))) for a in range(a0, a1 + 1, 6)]
        tx, ty = -math.sin(math.radians(a1)), math.cos(math.radians(a1))
        tip = arc[-1]
        lines.append(arc)
        for turn in (-0.8, 0.8):  # arms swept back from the tip
            c, s = math.cos(math.pi - turn), math.sin(math.pi - turn)
            lines.append([tip, (tip[0] + head * (tx * c - ty * s), tip[1] + head * (tx * s + ty * c))])
    return [(p, q) for line in lines for p, q in zip(line, line[1:])]


def seg_dist(px, py, a, b):
    ax, ay, bx, by = a[0], a[1], b[0], b[1]
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - ax - t * dx, py - ay - t * dy)


def render(n, R, fill, lift, badge):
    c = n / 2
    s = fill * R / REACH  # px per logo unit
    hx, hy = c, c - lift * R
    place = lambda x, y: (hx + (x - CENTRE[0]) * s, hy + (y - CENTRE[1]) * s)
    ring = shape(RING, n, place, BOLD * s)
    blades = union(shape(BLADES[0], n, place, BOLD * s), shape(BLADES[1], n, place, BOLD * s))
    glow = blur(union(ring, blades), n, max(1, round(0.043 * R)))
    qx, qy = place(*CROSS)
    strokes, bw, by = (sync_arrows(R), 0.052 * R, 0.69 * R) if badge else ([], 0, 0)
    px = []
    for j in range(n):
        for i in range(n):
            x, y = i + 0.5 - c, j + 0.5 - c
            d = math.hypot(x, y)
            alpha = max(0.0, min(1.0, R + 0.5 - d))
            if alpha == 0:
                px.append((0, 0, 0, 0))
                continue
            u, v, k = x / R, y / R, j * n + i
            # Sky lit from the middle, with faint light ripples from the blades' crossing.
            col = ramp(SKY, (v + 1) / 2)
            col = mix(col, WHITE, 0.95 * math.exp(-(u * u + (v + 0.02) ** 2) / 0.55))
            rr = math.hypot(i + 0.5 - qx, j + 0.5 - qy) / (0.236 * R)
            col = mix(col, WHITE, 0.26 * max(0.0, 1 - abs(rr % 1 - 0.5) * 6) * min(1.0, rr / 1.5))
            # The halo, on a soft white glow.
            col = mix(col, WHITE, 0.85 * min(1.0, glow[k] * 2))
            col = mix(col, INK, ring[k])
            col = mix(col, BLUE, blades[k])
            if badge:
                # Frosted cap (a big circle's top), bright at its edge, holding the sync arrows.
                edge = 1.67 * R - math.hypot(x, y - 2.06 * R)
                if edge > -1:
                    a = max(0.0, min(1.0, edge + 0.5))
                    col = mix(col, ramp(CAP, (v - 0.39) / 0.61), 0.88 * a)
                    col = mix(col, WHITE, 0.9 * math.exp(-(edge / (0.012 * R)) ** 2))
                if abs(x) < 0.3 * R and abs(y - by) < 0.3 * R:
                    dd = min(seg_dist(x, y - by, p, q) for p, q in strokes)
                    col = mix(col, NAVY, max(0.0, min(1.0, bw / 2 + 0.5 - dd)))
            # Glass: darker rim, soft highlight across the top.
            col = mix(col, RIM, 0.85 * max(0.0, (d / R - 0.86) / 0.14) ** 2)
            hl = (v + 0.62) / 0.42
            col = mix(col, WHITE, 0.25 * max(0.0, 1 - (u * u * 1.6 + hl * hl)))
            px.append((round(col[0] * 255), round(col[1] * 255), round(col[2] * 255), round(alpha * 255)))
    return px


def png(path, px, n):
    raw = b"".join(b"\0" + b"".join(bytes(p) for p in px[r * n:(r + 1) * n]) for r in range(n))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    n = APP[0]
    px = render(*APP)
    head = struct.pack("<BBHHHHH", 0x19, 0x10, 0, n, n, n * 4, 0)
    body = b"".join(bytes([p[2], p[1], p[0], p[3]]) for p in px)
    out = ROOT / "assets/appicon.bin"
    out.write_bytes(head + body)
    png(ROOT / "assets/appicon.png", px, n)
    png(ROOT / "plugins/astrobox-bal2d-import/icon.png", render(*PLUGIN), PLUGIN[0])
    print(out, len(head + body), "bytes")


main()
