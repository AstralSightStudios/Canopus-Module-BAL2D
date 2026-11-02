#!/usr/bin/env python3
"""End-to-end scenarios on the Mirage Band 10 Pro (043) device, the way a user meets them: the
character in a Lua watchface and in patched stock watchfaces, tap / pat interactions, switching
and deleting characters, the full-screen overlay on a watchface without a layer, screen off and
on, leaving the watchface and coming back, and a transfer while the character plays.

    scripts/scenarios.py PACK_DIR [--only NAME,...]

Screenshots, short recordings and a summary (summary.txt) go to review/mirage/scenarios/.
The packs in PACK_DIR are installed as needed (Shiroko, Hoshino, CH0335 and Yuuka are used).
"""
import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mirage_session as ms  # noqa: E402
from mirage_dev import Dev  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "review/mirage/scenarios"
LUA_FACE = "000000000B2D"
STOCK_FACE = "120917385439"  # 流沙


class Run:
    def __init__(self, packs):
        self.s = ms.Session("043", str(OUT / "shots"))
        self.dev = Dev(self.s.device, OUT)
        self.packs = Path(packs)
        self.lines = []
        self.ids = {}

    def log(self, text):
        print(text, flush=True)
        self.lines.append(text)

    def link(self, *args, tries=3):
        """bal2d-link-cli through the bridge; returns its stdout (retried: a page can time out)."""
        for attempt in range(tries):
            r = subprocess.run([sys.executable, str(ROOT / "scripts/mirage_session.py"), "043", "link", *map(str, args)],
                               capture_output=True, text=True, timeout=1800)
            if r.returncode == 0:
                return r.stdout
            self.log(f"  link {args[0]} failed (attempt {attempt + 1}): {r.stderr.strip().splitlines()[-1:] }")
            time.sleep(5)
        return r.stdout

    def face(self, face_id):
        self.s.phone('setWatchface={"id":"%s"}' % face_id)
        time.sleep(2)

    def install_face(self, path):
        out = self.s.phone('install={"path":"%s","kind":"watchface"}' % Path(path).resolve(), "watchfaces")
        return out

    def stats(self):
        out = self.link("stats")
        m = re.search(r"frames: (\d+), fps_x10: (\d+).*?errors: (\d+), heap_free_kib: (\d+), pack: (\d+)", out)
        if not m:
            return None
        return dict(frames=int(m[1]), fps=int(m[2]) / 10, errors=int(m[3]), heap=int(m[4]), pack=int(m[5]))

    def pack_id(self, name):
        if not self.ids:
            for line in self.link("list").splitlines():
                parts = line.split()
                if len(parts) >= 4 and parts[0].startswith("0x"):
                    self.ids[parts[-1]] = parts[0]
        return self.ids.get(name)

    def ensure(self, name, select=True):
        """Installs PACK_DIR/name.bal2d unless the watch has it; selects it."""
        pid = self.pack_id(name)
        if pid is None:
            out = self.link("install", self.packs / f"{name}.bal2d", *(["--select"] if select else []))
            self.log(f"  install {name}: {out.strip().splitlines()[-1] if out.strip() else '?'}")
            self.ids = {}
            pid = self.pack_id(name)
        elif select:
            self.link("select", pid)
        return pid

    def shot(self, name):
        path = self.dev.shot(name)
        self.log(f"  shot {path.relative_to(ROOT)} ({path.stat().st_size} B)")
        return path

    def record(self, name, seconds, *events):
        out = OUT / f"{name}.mp4"
        args = [sys.executable, str(ROOT / "scripts/record.py"), self.s.device, str(out), str(seconds)]
        for e in events:
            args += list(map(str, e))
        self.dev.wake()
        subprocess.run(args, capture_output=True, timeout=600)
        self.log(f"  recording {out.relative_to(ROOT)}")

    def check(self, label, ok, detail=""):
        self.log(f"{'PASS' if ok else 'FAIL'} {label}{(': ' + detail) if detail else ''}")
        return ok

    # -- scenarios ----------------------------------------------------------------------------
    def lua_face(self):
        self.face(LUA_FACE)
        self.ensure("Shiroko_home")
        time.sleep(14)
        a = self.stats()
        self.shot("lua-shiroko")
        time.sleep(4)
        b = self.stats()
        self.check("Lua face draws the character", a and b and b["frames"] > a["frames"] and b["fps"] > 0,
                   f"{a} -> {b}")

    def interactions(self):
        self.face(LUA_FACE)
        self.record("tap-talk", 8, ("--tap", 1, 168, 400))
        # Strokes below the system's pull-down handle (top centre, about 80 x 80 px).
        self.record("head-pat", 9, ("--swipe", 1, 110, 88, 230, 88, 1200), ("--swipe", 3.5, 230, 88, 110, 88, 1200),
                    ("--swipe", 6, 110, 88, 230, 88, 1200))
        st = self.stats()
        self.check("interactions keep rendering without errors", st and st["errors"] == 0, str(st))

    def switch(self):
        before = self.stats()
        pid = self.ensure("Hoshino_home")
        time.sleep(12)
        after = self.stats()
        self.shot("switch-hoshino")
        self.check("switching characters loads the new pack", after and pid and after["pack"] == int(pid, 16),
                   f"{before} -> {after}")

    def marker_faces(self):
        for name in ["island", "cat"]:
            path = ROOT / f"build/mirage/faces/{name}-bal2d.bin"
            # The patched face's own package id (BIN header +0x28): a copy from an earlier run is
            # removed first (the phone installs every face with one version, so it cannot replace).
            face_id = path.read_bytes()[0x28:0x48].split(b"\0")[0].decode()
            listed = self.s.phone("watchfaces")
            if f'"id":"{face_id}"' in listed:
                self.s.phone('removeWatchface={"id":"%s"}' % face_id)
            self.install_face(path)
            self.face(face_id)
            self.log(f"  installed {name} as {face_id}")
            time.sleep(10)
            st0 = self.stats()
            self.shot(f"marker-{name}")
            time.sleep(4)
            st1 = self.stats()
            self.check(f"patched face '{name}' shows the character in its layer",
                       st0 and st1 and st1["frames"] > st0["frames"], f"{st0} -> {st1}")

    def overlay(self):
        self.face(STOCK_FACE)
        time.sleep(3)
        a = self.stats()
        self.shot("stock-face-no-layer")
        time.sleep(3)
        b = self.stats()
        idle = a and b and b["frames"] == a["frames"]
        self.check("a face without a layer draws nothing (no overlay)", idle, f"{a} -> {b}")
        self.link("options", 1)
        time.sleep(6)
        c = self.stats()
        self.shot("stock-face-overlay")
        time.sleep(3)
        d = self.stats()
        self.check("full-screen option draws over a face without a layer", c and d and d["frames"] > c["frames"],
                   f"{c} -> {d}")
        # The layer belongs to the watchface: the launcher is reachable and not covered.
        self.dev.wake()
        self.dev.swipe(168, 300, 168, 60, 300, 2)
        e = self.stats()
        self.shot("stock-face-overlay-launcher")
        time.sleep(3)
        f = self.stats()
        self.check("the full-screen layer stays on the watchface (launcher reachable, nothing drawn)",
                   e and f and f["frames"] - e["frames"] <= 2, f"{e} -> {f}")
        self.dev.swipe(168, 240, 168, 470, 300, 2)  # launcher -> watchface
        self.link("options", 0)
        time.sleep(4)
        self.shot("stock-face-overlay-off")

    def to_face(self):
        """Back on the watchface if a page is left open (a lost gesture can leave the launcher up):
        when no frames are drawn, swipe down from the middle (the launcher's way back)."""
        self.dev.wake()
        a = self.stats()
        time.sleep(2)
        b = self.stats()
        if a and b and b["frames"] == a["frames"]:
            self.log("  not drawing: swiping back to the watchface")
            self.dev.swipe(168, 240, 168, 470, 300, 2)

    def pages(self):
        """Home pages and the pull-down notifications slide over the watchface without hiding it
        (LVGL keeps drawing it under them): nothing is drawn while one covers it, drawing resumes
        after. The pull starts at the top edge over the character's head (x 250): the system edge
        must still be the system's there."""
        self.face(LUA_FACE)
        self.ensure("Shiroko_home")
        time.sleep(8)
        for label, there, back in (("the next home page", (320, 440, 40, 440), (40, 440, 320, 440)),
                                   ("the previous home page", (40, 440, 320, 440), (320, 440, 40, 440)),
                                   ("the pull-down notifications", (250, 5, 250, 400), (250, 470, 250, 30))):
            self.dev.wake()
            a = self.stats()
            time.sleep(3)
            b = self.stats()
            self.dev.swipe(*there, 300, 2)
            c = self.stats()
            time.sleep(3)
            d = self.stats()
            if c and d and d["frames"] - c["frames"] > 2:
                # The gesture can be lost (a phone-link call just before it can leave the top edge
                # busy): try it once more. A page that opened but was not detected still fails.
                self.log(f"  {label}: frames still advancing, swiping again")
                self.dev.wake()
                time.sleep(2)
                self.dev.swipe(*there, 300, 2)
                c = self.stats()
                time.sleep(3)
                d = self.stats()
            self.shot("page-" + label.split()[-2] + "-" + label.split()[-1])
            self.dev.swipe(*back, 300, 2)
            time.sleep(1)
            e = self.stats()
            time.sleep(3)
            f = self.stats()
            ok = all((a, b, c, d, e, f)) and b["frames"] > a["frames"] and d["frames"] - c["frames"] <= 2 \
                and f["frames"] > e["frames"]
            if not ok:
                self.to_face()
            self.check(f"nothing is drawn under {label}, drawing resumes after", ok,
                       f"{a and a['frames']}->{b and b['frames']} | {c and c['frames']}->{d and d['frames']} | "
                       f"{e and e['frames']}->{f and f['frames']}")

    def screen_off_on(self):
        self.face(LUA_FACE)
        time.sleep(4)
        self.to_face()
        self.dev.wake()
        a = self.stats()
        time.sleep(12)  # the screen goes off after ~5 s
        b = self.stats()
        self.dev.wake()
        time.sleep(3)
        c = self.stats()
        self.shot("after-wake")
        time.sleep(2)
        d = self.stats()
        if self.dev.lit():
            self.log("SKIP no drawing while the screen is off: the screen is held on (keepawake.flag)")
        else:
            off = b and a and (b["frames"] - a["frames"]) < 30
            self.check("no drawing while the screen is off", off, f"{a} -> {b}")
        self.check("drawing resumes after waking", c and d and d["frames"] > c["frames"], f"{c} -> {d}")

    def leave_and_return(self):
        self.face(STOCK_FACE)
        time.sleep(4)
        self.face(LUA_FACE)
        time.sleep(6)
        self.to_face()
        a = self.stats()
        self.shot("returned-to-face")
        time.sleep(3)
        b = self.stats()
        self.check("the layer re-attaches after switching watchfaces", a and b and b["frames"] > a["frames"],
                   f"{a} -> {b}")

    def transfer_while_playing(self):
        self.face(LUA_FACE)
        self.to_face()
        pid = self.pack_id("CH0335_home")
        if pid:
            self.link("delete", pid)
            self.ids = {}
        a = self.stats()
        t0 = time.time()
        out = self.link("install", self.packs / "CH0335_home.bal2d")
        dt = time.time() - t0
        b = self.stats()
        self.shot("after-transfer")
        self.ids = {}
        ok = self.pack_id("CH0335_home") is not None
        self.log(f"  {out.strip().splitlines()[-1] if out.strip() else '?'}")
        self.check("a transfer completes while the character plays",
                   ok and a and b and b["errors"] == 0 and b["frames"] > a["frames"],
                   f"{dt:.1f} s; {a} -> {b}")

    def delete(self):
        pid = self.ensure("Yuuka_home", select=False)
        out = self.link("delete", pid)
        self.ids = {}
        gone = self.pack_id("Yuuka_home") is None
        self.check("deleting a character removes it from the watch", gone, out.strip())

    def app_page(self):
        """The watch app (last launcher page): its status, and "next character"."""
        before = self.stats()
        self.dev.launcher_to_end()
        self.dev.tap(55, 288, 4)  # the app's icon on this emulator's launcher
        self.shot("app-page")
        # The watchface window is hidden under the app: nothing is drawn meanwhile.
        g = self.stats()
        time.sleep(3)
        h = self.stats()
        self.check("nothing is drawn while an app covers the watchface", g and h and h["frames"] - g["frames"] <= 2,
                   f"{g} -> {h}")
        self.dev.tap(168, 112, 1)  # 下一个角色
        time.sleep(10)
        self.dev.wake()
        self.shot("app-page-next")
        after = self.stats()
        self.check("the app's 'next character' switches the pack", before and after and after["pack"] != before["pack"],
                   f"{before} -> {after}")
        # Back to the watchface: the home button only goes back as far as the launcher here; two
        # back swipes (app -> launcher -> watchface) close the app.
        self.dev.wake()
        self.dev.swipe(5, 240, 300, 240, 300, 2)
        self.dev.swipe(5, 240, 300, 240, 300, 2)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("packs")
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    r = Run(a.packs)
    names = ["lua_face", "interactions", "switch", "marker_faces", "overlay", "pages", "screen_off_on",
             "leave_and_return", "transfer_while_playing", "delete", "app_page"]
    only = [n for n in a.only.split(",") if n]
    for name in names:
        if only and name not in only:
            continue
        r.log(f"== {name}")
        try:
            getattr(r, name)()
        except Exception as error:
            r.check(name, False, f"exception {error!r}")
    (OUT / "summary.txt").write_text("\n".join(r.lines) + "\n")
    fails = sum(1 for l in r.lines if l.startswith("FAIL"))
    print(f"\n{fails} failed; summary in {OUT / 'summary.txt'}")


if __name__ == "__main__":
    main()
