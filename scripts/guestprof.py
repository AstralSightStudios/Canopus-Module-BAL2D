#!/usr/bin/env python3
"""Symbolizes Mirage guest-time PC samples (QEMU MIRAGE_PROFILE dumps, see qemu-miwear
hw/arm/mirage_profile.c) for the BAL2D module and the 043 firmware.

    scripts/guestprof.py BEFORE AFTER [--top N]

BEFORE / AFTER are two dumps of one run; the profile is their difference. The module's load
address is found by matching guest memory at its hottest PC against the ELF's .text.
"""
import argparse, collections, json, struct, sys, bisect, subprocess
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
from mirage_mem import qmp, words  # noqa: E402

ELF = ROOT / "build/xiaomi-band-10-pro-3.101.043/ba_l2d.elf"
CORPUS = Path("/Volumes/EXT0/Canopus/targets/fw-corpus/xiaomi-band-10-pro-3.101.043.json")


def load(p):
    d = {}
    for l in open(p):
        if l.startswith('#'):
            continue
        pc, lr, n = l.split()
        d[(int(pc, 16), int(lr, 16))] = int(n)
    return d


def elf_text(path):
    b = path.read_bytes()
    shoff, = struct.unpack_from('<I', b, 0x20)
    shentsize, shnum, shstrndx = struct.unpack_from('<HHH', b, 0x2E)
    secs = [struct.unpack_from('<IIIIIIIIII', b, shoff + i * shentsize) for i in range(shnum)]
    strtab = secs[shstrndx]
    name = lambda off: b[strtab[4] + off:b.index(b'\0', strtab[4] + off)].decode()
    for s in secs:
        if name(s[0]) == '.text':
            return b[s[4]:s[4] + s[5]]


def module_symbols():
    out = subprocess.run(["nm", "-n", "--defined-only", "-C", str(ELF)], capture_output=True, text=True).stdout
    syms = []
    for line in out.splitlines():
        parts = line.split(' ', 2)
        if len(parts) == 3 and parts[1] in 'tT':
            syms.append((int(parts[0], 16) & ~1, parts[2]))
    syms.sort()
    return syms


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--device", default="bal2d-dev-043")
    ap.add_argument("--func", help="module function (substring): its hottest instructions, disassembled")
    a = ap.parse_args()
    A, B = load(a.before), load(a.after)
    diff = {k: B[k] - A.get(k, 0) for k in B if B[k] - A.get(k, 0) > 0}
    total = sum(diff.values())
    text = elf_text(ELF)
    # Module base: the hottest PC in PSRAM, matched against .text.
    pcs = collections.Counter()
    for (pc, lr), n in diff.items():
        pcs[pc] += n
    hmp = qmp(a.device)
    base = None
    for pc, _ in pcs.most_common(50):
        if not (0x3C000000 <= pc < 0x3E000000):
            continue
        start = (pc & ~3) - 16
        mem = b''.join(struct.pack('<I', w) for w in words(hmp, start, 12))
        i = text.find(mem[:24])
        if i >= 0 and text.find(mem[:24], i + 1) < 0:
            base = start - i
            break
    print(f"module .text at {base:#x}" if base else "module base not found")
    syms = module_symbols()
    addrs = [s[0] for s in syms]
    corpus = json.load(open(CORPUS))
    fw = sorted((int(f["addr"], 16), f["size"], (f.get("strings") or [""])[0][:50]) for f in corpus["functions"])
    fwa = [f[0] for f in fw]

    def where(pc):
        if base and base <= pc < base + len(text):
            off = pc - base
            i = bisect.bisect_right(addrs, off) - 1
            return "mod " + (syms[i][1][:90] if i >= 0 else "?")
        p = pc
        if 0x1C000000 <= pc < 0x1D000000:
            p = pc - 0x1C000000 + 0x0C10D440
        i = bisect.bisect_right(fwa, p) - 1
        if i >= 0 and fw[i][0] <= p < fw[i][0] + max(fw[i][1], 1):
            return f"fw {fw[i][0]:#x} {fw[i][2]}"
        if pc < 0x01000000:
            return f"ramx {pc & ~0xFF:#x}"
        return f"? {pc & ~0xFFF:#x}"

    by = collections.Counter()
    for pc, n in pcs.items():
        by[where(pc)] += n
    mod = sum(n for k, n in by.items() if k.startswith("mod "))
    print(f"{total} samples; module {100 * mod / total:.1f}%")
    for k, n in by.most_common(a.top):
        print(f"{100 * n / total:5.1f}%  {k}")
    if a.func and base:
        import capstone
        md = capstone.Cs(capstone.CS_ARCH_ARM, capstone.CS_MODE_THUMB | capstone.CS_MODE_MCLASS)
        md.skipdata = True
        for i, (addr, name) in enumerate(syms):
            if a.func not in name:
                continue
            addr &= ~1
            end = (syms[i + 1][0] & ~1) if i + 1 < len(syms) else len(text)
            hot = collections.Counter()
            for pc, n in pcs.items():
                if base + addr <= pc < base + end:
                    hot[pc - base - addr] += n
            fn = sum(hot.values())
            if not fn:
                continue
            print(f"\n{name[:100]} ({end - addr} bytes, {100 * fn / total:.1f}%)")
            ins = {x.address: x for x in md.disasm(text[addr:end], addr)}
            # Windows of instructions around the hottest offsets.
            shown = set()
            for off, n in hot.most_common(12):
                off &= ~1
                for o in range(off - 12, off + 4, 2):
                    x = ins.get(addr + o)
                    if x is None or o in shown:
                        continue
                    shown.add(o)
                    c = hot.get(o, 0)
                    print(f"  +{o:5x} {100 * c / total:5.2f}%  {x.mnemonic} {x.op_str}")
                print("  ...")


if __name__ == "__main__":
    main()
