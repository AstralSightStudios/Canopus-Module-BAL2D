#!/usr/bin/env python3
"""A long-lived Mirage device for developing BAL2D: build, deploy, reboot, run.

    mirage_session.py KEY create    fresh device (KEY: 043 = Band 10 Pro, the one target): pair, install the
                                    production installer face
    mirage_session.py KEY cycle     package this build for the device's target (bumped version_code), install it
                                    over CPC2 while the Supervisor is loaded, power cycle, Run
    mirage_session.py KEY deploy    like cycle, but from any device state: power cycle, Run, install, power cycle
    mirage_session.py KEY reset-data   (device off) remove the module's data directory: next boot re-extracts the pack
    mirage_session.py KEY health STEPS [BPM]   (device off) write the simulated health reading
    mirage_session.py KEY restart   power cycle and Run again (module state is kept)
    mirage_session.py KEY enable    enable the module (new installs are disabled); takes effect at the next boot
    mirage_session.py KEY enable-ui the same by clicking through the Manager (positions measured on old layouts)
    mirage_session.py KEY launch    launcher -> end (screenshots land in --shots)

Reuses the Canopus emulator tooling (installer build, Facetory packager, AstroBox bridge).
BAL2D_DEVICE_<KEY>=NAME points a KEY at another emulator device (a fresh one, say) with its own work directory.
The AstroBox bridge has to speak the Supervisor's CPC2 ABI (2.0 since Canopus' ABI 2.0 change), or every phone
request times out silently. Emulator evidence only.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
from mirage_dev import Dev, MIRAGECTL  # noqa: E402

CANOPUS = Path(os.environ.get("CANOPUS_ROOT", ROOT / "../Canopus")).resolve()
BRIDGE = Path(os.environ.get("MAB_SMOKE", "/Volumes/EXT0/Mirage/AstroBoxBridge/target/smoke"))
PACKAGER = Path(os.environ.get("FACETORY_PACKAGER",
                               "/Volumes/EXT0/GitHub/facetory/rust/target/debug/examples/package_lua_watchface"))
CLI = CANOPUS / "target/debug/canopus"
WORK = Path(os.environ.get("BAL2D_DEV_WORK", ROOT / "build/mirage"))

PROD_INSTALLER = CANOPUS / "watchfaces/canopus-installer-prod/xiaomi-band-10-pro"

PROFILES = {
    # Band 10 Pro: a quick double tap wakes the panel; the installer is the prebuilt production face.
    "043": dict(device="bal2d-dev-043", target="xiaomi-band-10-pro-3.101.043", size=(336, 480),
                firmware="miwear.watch.p67tc_v3.101.043_full_a4ce8564",
                pair_confirm=(250, 435), run=(168, 168), pair_wake=(168, 240), wake=[(168, 240), (168, 240)],
                idle=(168, 440), run_taps=(16, 2.2),
                manager=dict(open=(168, 450, 168, 60), scroll=(12, (168, 440, 168, 60)), icon=(282, 288),
                             home_scroll=(2, (168, 420, 168, 120)), modules_row=(168, 155), first_module=(168, 100),
                             scroll_top=(2, (168, 150, 168, 440)), third_row=(168, 340), confirm=(168, 410), wait=3)),
}


def sh(*args, **kw):
    kw.setdefault("check", True)
    return subprocess.run([str(a) for a in args], **kw)


class Session:
    def __init__(self, key, shots):
        # BAL2D_DEVICE_<key> points the same scripts at another emulator device (a fresh one, say)
        # without touching the long-lived default; it gets its own work directory.
        override = os.environ.get(f"BAL2D_DEVICE_{key}")
        self.p = dict(PROFILES[key], device=override) if override else PROFILES[key]
        self.key = key
        self.device = self.p["device"]
        self.work = WORK / (override or key)
        self.work.mkdir(parents=True, exist_ok=True)
        self.dev = Dev(self.device, shots) if self.exists() else None
        self.shots = shots

    def ctl(self, *args, check=True):
        r = subprocess.run([str(MIRAGECTL), *map(str, args)], capture_output=True, text=True)
        if check and r.returncode != 0:
            raise RuntimeError(f"miragectl {args}: {r.stderr or r.stdout}")
        return r.stdout

    def exists(self):
        return self.device in subprocess.run([str(MIRAGECTL), "list"], capture_output=True, text=True).stdout

    # -- phone side ---------------------------------------------------------------------------
    def phone(self, *methods, state=None, retried=False):
        info = self.ctl("info", self.device)
        addr = [l.split()[-1] for l in info.splitlines() if l.startswith("bt addr")][0]
        key = [l.split()[-1] for l in info.splitlines() if l.startswith("bound")][0]
        port = [l for l in self.ctl("list").splitlines() if self.device in l][0].split("BT :")[1].rstrip(")")
        state = state or self.work / "abstate"
        proc = subprocess.Popen([str(BRIDGE), port, key, str(state), *methods], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, env=dict(os.environ, MAB_BAND=addr))
        if not (Path(state) / "link-keys").exists():
            # The band declines a pairing request that is not confirmed within about 8 s.
            time.sleep(3)
            for point in self.p["wake"]:
                self.ctl("tap", self.device, *point, check=False)
            for _ in range(8):
                time.sleep(1.5)
                self.ctl("tap", self.device, *self.p["pair_confirm"], check=False)
        out, _ = proc.communicate(timeout=600)
        (self.work / f"bridge-{time.time_ns()}.log").write_text(out)
        if "pairing was declined" in out and (Path(state) / "link-keys").exists() and not retried:
            # The band forgot the bond (a quick power-off after pairing): pair afresh, confirming it.
            (Path(state) / "link-keys").rename(Path(state) / f"link-keys.stale-{int(time.time())}")
            return self.phone(*methods, state=state, retried=True)
        if proc.returncode != 0 or '"ok":false' in out:
            raise RuntimeError("bridge failed:\n" + out[-3000:])
        return out

    def link(self, *command, timeout=1800):
        """Runs the PetLink driver (tools/pet-link-cli) against this device: the phone side of the
        AstroBox plugin (same sender, same pack preparation) through the bridge."""
        info = self.ctl("info", self.device)
        addr = [l.split()[-1] for l in info.splitlines() if l.startswith("bt addr")][0]
        key = [l.split()[-1] for l in info.splitlines() if l.startswith("bound")][0]
        port = [l for l in self.ctl("list").splitlines() if self.device in l][0].split("BT :")[1].rstrip(")")
        sh("cargo", "build", "-q", "-p", "bal2d-link-cli", cwd=ROOT)
        tool = ROOT / "target/debug/bal2d-link-cli"
        r = subprocess.run([str(tool), "--smoke", str(BRIDGE), "--port", port, "--key", key,
                            "--state", str(self.work / "abstate"), "--band", addr, *map(str, command)],
                           capture_output=True, text=True, timeout=timeout)
        print(r.stdout, end="")
        print(r.stderr[-3000:], end="", file=sys.stderr)
        return r.returncode == 0

    # -- installer ------------------------------------------------------------------------------
    def build_installer(self):
        face = self.work / "face"
        shutil.rmtree(face, ignore_errors=True)
        face.mkdir()
        names = ["main.lua", "manager_icon.bin", f"canopus_supervisor-{self.p['target']}.bin",
                 f"canopus_boot-{self.p['target']}.bin"]
        for name in names:
            shutil.copy(PROD_INSTALLER / name, face / name)
        png(face / "preview.png", *self.p["size"])
        sh(PACKAGER, face, "--name", "Canopus", "--package-id", "000000000031", "--width", self.p["size"][0],
           "--height", self.p["size"][1], "-o", self.work / "installer.bin", capture_output=True)

    def create(self):
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        self.ctl("delete", self.device, check=False)
        shutil.rmtree(self.work / "abstate", ignore_errors=True)
        self.ctl("create", self.device, "--firmware", self.p["firmware"])
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(60)
        self.build_installer()
        self.phone('install={"path":"%s","kind":"watchface"}' % (self.work / "installer.bin"),
                   'setWatchface={"id":"000000000031"}', "watchfaces")
        time.sleep(8)
        self.dev = Dev(self.device, self.shots)
        self.run_installer()

    def installer_face(self):
        """The installer watchface must be current for Run (recordings switch to a stock face)."""
        try:
            self.phone('setWatchface={"id":"000000000031"}')
            time.sleep(3)
        except RuntimeError as error:
            print(f"(installer face not set: {str(error)[:200]})")

    def face(self, face_id):
        """Makes `face_id` the current watchface (ids: `watchfaces` through the bridge)."""
        self.phone('setWatchface={"id":"%s"}' % face_id)

    def run_installer(self):
        """The installer face is the current watchface: wake, Run, wait for the Supervisor and the
        module restore (which registers the launcher entry)."""
        self.dev.wake()
        self.dev.tap(*self.p["run"], 1)
        # The loaded Supervisor's timers stall while the screen is off: keep poking an inert spot.
        count, interval = self.p["run_taps"]
        for _ in range(count):
            time.sleep(interval)
            self.dev.tap(*self.p["idle"], 0.2)
        self.dev.wake()

    # -- module ---------------------------------------------------------------------------------
    def package(self, bump=True):
        """Builds and packages the module for this device's target only, with a version code that
        grows on every call so the Supervisor takes it as an update (`bump=False` keeps the
        installed one, for `fast`)."""
        text = (ROOT / "Canopus.toml").read_text()
        counter = self.work / "version_code"
        code = int(counter.read_text()) + (1 if bump else 0) if counter.exists() else 1000
        counter.write_text(str(code))
        text = re.sub(r"(?m)^version_code = \d+", f"version_code = {code}", text)
        text = re.sub(r"(?s)\[targets\]\ninclude = \[.*?\]", f'[targets]\ninclude = ["{self.p["target"]}"]', text)
        manifest = ROOT / f"Canopus.dev-{self.key}.toml"
        manifest.write_text(text)
        output = self.work / "ba_l2d.canopus"
        try:
            sh(CLI, "build", "--manifest", manifest, cwd=ROOT, capture_output=True)
            sh(CLI, "package", "--manifest", manifest, "--output", output, cwd=ROOT, capture_output=True)
        finally:
            manifest.unlink(missing_ok=True)
        return output

    def install_module(self):
        """CPC2 install of the freshly packaged module; an update of a resident module answers
        REBOOT_REQUIRED, which the bridge reports as success."""
        package = self.package()
        out = self.phone('install={"path":"%s","kind":"canopus"}' % package, "canopusModules")
        print(out[-500:])

    def restart(self, seed=0):
        """Power cycle, then Run."""
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(22)
        self.dev = Dev(self.device, self.shots)
        self.installer_face()
        self.run_installer()

    def cycle(self, seed=0):
        self.install_module()
        self.restart()

    def deploy(self, seed=0):
        """From any state: power cycle, Run (the Supervisor must be loaded for a CPC2 install), install
        the current build, power cycle again with the library re-seeded and Run once more."""
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(48)
        self.dev = Dev(self.device, self.shots)
        self.installer_face()
        self.run_installer()
        self.install_module()
        self.restart()

    def enable(self):
        """A freshly installed module is disabled. Enable it with the CPC2 command the Manager's own
        "启用" row sends (the same call AstroBox makes); it takes effect at the next boot, so `restart`
        afterwards. The Manager walk below clicked rows by position and broke whenever the Manager's page
        layout changed (it once hit "移除" and removed the module)."""
        self.phone('canopusSetEnabled={"module":"ba_l2d","enabled":true}')

    def enable_manager_walk(self):
        """Enables and activates the module by clicking through the Manager. Walk the Manager (launcher > Manager > modules > the
        module > enable > activate now); coordinates measured on Mirage (see Canopus scripts/emu/ui_pages.py).
        The screen sleeps a few seconds after the last touch, so every step is a touch. On Band 10 Pro the
        Manager icon is the last launcher entry only until Lyra's own icon exists."""
        m = self.p["manager"]
        d = self.dev
        d.wake()

        def swipe(gesture, settle=2.5):
            self.ctl("swipe", self.device, *gesture, 300)
            time.sleep(settle)

        def tap(point, settle=0.6):
            self.ctl("tap", self.device, *point)
            time.sleep(settle)

        def body():
            probe = self.work / "enable-probe.png"
            self.ctl("screenshot", self.device, probe)
            return probe.read_bytes()

        swipe(m["open"], 3)
        count, gesture = m["scroll"]
        for _ in range(count):
            swipe(gesture, 0.4)
        tap(m["icon"], 4)
        before = body()
        count, gesture = m["home_scroll"]
        for _ in range(40):
            swipe(gesture, 2.5)
            if body() != before:
                break
        for _ in range(count - 1):
            swipe(gesture, 2.5)
        tap(m["modules_row"], 3.5)
        tap(m["first_module"], 3.5)

        def to_top():
            if m["scroll_top"]:
                for _ in range(m["scroll_top"][0]):
                    swipe(m["scroll_top"][1], 2.5)

        to_top()
        for step, wait in (("enable", 3), ("activate", m["wait"])):
            tap(m["third_row"], 1.5)
            d.shot("dialog-" + step)
            tap(m["confirm"], wait)
            to_top()
        d.shot("module-running")

    def launcher(self):
        """Launcher -> scroll to the end."""
        self.dev.launcher_to_end()
        self.dev.shot("launcher")

    DATA = "/canopus/m.ba_l2d"

    def fast(self):
        """Development shortcut: rebuild with the installed version code and replace the module
        image and its receipt in the Supervisor's inbox while the device is off, then boot and Run.
        Avoids the 90 s CPC2 transfer; the receipt is the package's own."""
        import tarfile
        package = self.package(bump=False)
        out = self.work / "fast"
        shutil.rmtree(out, ignore_errors=True)
        with tarfile.open(package) as tar:
            tar.extractall(out)
        elf = out / f"artifacts/{self.p['target']}/module.elf"
        cmi = out / f"receipts/{self.p['target']}.cmi"
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        # The Supervisor keeps two generations (A = .ko/.cmi, B = .ko.b/.cmi.b) and loads the active
        # one; a CPC2 update leaves B active. Same version code in both: whichever runs is this one.
        inbox = self.ctl("fs", "ls", self.device, "nand_data", "/canopus/inbox", check=False)
        for suffix in ["", ".b"] if "ba_l2d.ko.b" in inbox else [""]:
            self.ctl("fs", "put", self.device, "nand_data", elf, f"/canopus/inbox/ba_l2d.ko{suffix}")
            self.ctl("fs", "put", self.device, "nand_data", cmi, f"/canopus/inbox/ba_l2d.cmi{suffix}")
        # What the package installer would extract next to the module (Canopus.toml [resources]).
        self.ctl("fs", "mkdir", self.device, "nand_data", self.DATA, check=False)
        for resource in sorted((out / "resources/common").glob("*")):
            self.ctl("fs", "put", self.device, "nand_data", resource, f"{self.DATA}/{resource.name}")
        self.ctl("fs", "rm", self.device, "nand_data", f"{self.DATA}/log.txt", check=False)
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(48)
        self.dev = Dev(self.device, self.shots)
        self.installer_face()
        self.run_installer()

    def reset_data(self):
        """With the device off: drop the module's data (pack, saves, log)."""
        self.ctl("fs", "rm", self.device, "nand_data", self.DATA, check=False)

    def health(self, steps, bpm=None):
        """With the device off: the simulated health reading the engine prefers over sensors."""
        path = self.work / "health.lua"
        hr = f", heart_rate = {bpm}" if bpm else ""
        path.write_text(f"return {{ steps = {steps}{hr} }}\n")
        self.ctl("fs", "mkdir", self.device, "nand_data", self.DATA, check=False)
        self.ctl("fs", "put", self.device, "nand_data", path, f"{self.DATA}/health.lua")

    def pull(self, name, out):
        """Copies a file of the module's data directory off the (stopped) device."""
        return self.ctl("fs", "get", self.device, "nand_data", f"{self.DATA}/{name}", out, check=False)


def png(path, w, h):
    import struct
    import zlib
    rows = b"".join(b"\0" + bytes([40, 90, 200]) * w for _ in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                           + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("key", choices=list(PROFILES))
    ap.add_argument("cmd", choices=["create", "cycle", "deploy", "restart", "launch", "package", "install", "enable",
                                    "enable-ui", "reset-data", "health", "fast", "log", "link", "face"])
    ap.add_argument("args", nargs="*")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--shots", default=os.environ.get("MIRAGE_SHOTS", str(WORK / "shots")))
    a, extra = ap.parse_known_args()
    a.args += extra
    s = Session(a.key, a.shots)
    if a.cmd in ("cycle", "deploy", "restart"):
        getattr(s, a.cmd)(seed=a.seed)
    elif a.cmd == "log":
        s.ctl("shutdown", s.device, check=False)
        time.sleep(3)
        print(s.ctl("fs", "get", s.device, "nand_data", f"{s.DATA}/log.txt", check=False))
    elif a.cmd == "face":
        s.face(a.args[0])
    elif a.cmd == "link":
        sys.exit(0 if s.link(*a.args) else 1)
    elif a.cmd == "reset-data":
        s.reset_data()
    elif a.cmd == "health":
        s.health(int(a.args[0]), int(a.args[1]) if len(a.args) > 1 else None)
    else:
        getattr(s, {"launch": "launcher", "install": "install_module",
                    "enable-ui": "enable_manager_walk"}.get(a.cmd, a.cmd))()


if __name__ == "__main__":
    main()
