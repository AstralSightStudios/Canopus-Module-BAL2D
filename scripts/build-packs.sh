#!/usr/bin/env bash
# Development: compiles the reference lobbies into review/packs/<name>/ (assets are the user's own
# extraction and never part of this repository).
#   scripts/build-packs.sh v5 [extra bal2d-tool compile options...]
set -euo pipefail
cd "$(dirname "$0")/.."
A=${BAL2D_ASSETS:-/Volumes/EXT0/tmp/bal2d-ext/assets}
OUT=review/packs/$1; shift
mkdir -p "$OUT"
cargo build -q --release -p bal2d-tool
compile() {
    local name=$1; shift
    local args=()
    for s in "$@"; do args+=(--skel "$A/$s"); done
    printf '%-14s %s\n' "$name" "$(target/release/bal2d-tool compile "${args[@]}" --size 336x480 --format argb8565 \
        --budget 800000 ${EXTRA:-} --out "$OUT/$name.bal2d" 2>&1 | grep -E 'baked|error' | sed 's/^ *//' | head -1)"
}
for n in Shiroko_home Hina_home Yuuka_home Neru_home CH0335_home CH0348_home; do compile $n $n/$n.skel; done
compile Hoshino_home Hoshino_home/Hoshino_home_background/Hoshino_home_background.skel Hoshino_home/Hoshino_home/Hoshino_home.skel
compile CH0184_home CH0184_home/CH0184_00/CH0184_00.skel CH0184_home/CH0184_home.skel
ls -l "$OUT" | awk 'NR>1 {printf "  %-24s %8d\n", $9, $5}'
