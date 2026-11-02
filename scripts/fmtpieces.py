#!/usr/bin/env python3
"""Lists format-string literal pieces that the pack verifier can mistake for firmware addresses.

    scripts/fmtpieces.py [DIR ...]   (default: the crates the watch build compiles)

The compiler stores each literal piece of a format string after a one-byte length. A 12-byte
piece has length byte 0x0C. When that byte lands at the top of an aligned 32-bit word, the
word reads 0x0Cxxxxxx: inside the 043 firmware range. The Canopus verifier (CAN-P1-011) then
rejects the build with "embedded absolute address .rodata+... not in target pack allowlist".
Any edit that shifts .rodata can move a piece onto such a word, so keep pieces off 12 bytes
(and 28, 0x1C, the relocated LVGL range).
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIRS = ["crates/bal2d-device", "crates/bal2d-core/src", "crates/bal2d-render/src", "crates/bal2d-spine/src",
        "crates/bal2d-proto/src", "crates/bal2d-pack/src"]


def main():
    found = 0
    for d in sys.argv[1:] or DIRS:
        for path in sorted((ROOT / d).rglob("*.rs")):
            for n, line in enumerate(path.read_text().splitlines(), 1):
                for m in re.finditer(r'"((?:[^"\\]|\\.)*)"', line):
                    s = m.group(1)
                    if "{" not in s:
                        continue
                    for p in re.split(r"\{[^{}]*\}", s.replace("{{", "\0").replace("}}", "\0")):
                        size = len(re.sub(r"\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.", "_", p).encode())
                        if size in (12, 28):
                            found += 1
                            print(f"{path.relative_to(ROOT)}:{n}: {size}-byte piece {p!r}")
    sys.exit(1 if found else 0)


if __name__ == "__main__":
    main()
