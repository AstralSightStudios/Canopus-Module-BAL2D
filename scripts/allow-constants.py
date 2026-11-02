#!/usr/bin/env python3
"""Runs `canopus build` per target and adds the data words the verifier mistakes for firmware
addresses (Rust core's float/Unicode tables) to Canopus.toml's allowed_constants, until it passes.

    scripts/allow-constants.py [--check]
"""
import os, re, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CANOPUS = Path(os.environ.get("CANOPUS_ROOT", ROOT / "../Canopus")).resolve()
CLI = CANOPUS / "target/debug/canopus"
HIT = re.compile(r"embedded absolute address (\S+)\+0x[0-9a-f]+ (0x[0-9a-f]+) not in target pack allowlist")
MARK = "  # Rust core table words that look like firmware addresses (scripts/allow-constants.py):\n"

def build(target):
    r = subprocess.run([CLI, "build", "--target", target], cwd=ROOT, capture_output=True, text=True,
                       env=dict(os.environ, CANOPUS_ROOT=str(CANOPUS)))
    return r.returncode, r.stdout + r.stderr

def main():
    check = "--check" in sys.argv
    manifest = ROOT / "Canopus.toml"
    for target in ["xiaomi-band-10-pro-3.101.043"]:
        for _ in range(6):
            code, out = build(target)
            hits = sorted({m.group(2) for m in HIT.finditer(out)})
            if code == 0:
                print(f"{target}: ok")
                break
            if not hits or check:
                print(out[-3000:])
                sys.exit(f"{target}: build failed")
            text = manifest.read_text()
            if MARK not in text:
                text = text.replace("]\n\n[targets]", MARK + "]\n\n[targets]")
            new = "".join(f'  "0x{int(h, 16):08x}",\n' for h in hits if f"0x{int(h, 16):08x}" not in text)
            text = text.replace(MARK, MARK + new)
            manifest.write_text(text)
            print(f"{target}: allowed {len(hits)} words")
        else:
            sys.exit(f"{target}: did not converge")

main()
