#!/usr/bin/env bash
# Converts every .svg under assets/ to a .png next to it with Inkscape
# (the game loads PNGs; *.png is gitignored, so run this after checking
# out and after running generate.py or editing an SVG).
#
# Usage: assets/svg_to_png.sh [--force]
#   Only converts SVGs that are newer than their PNG, unless --force.
# Run it before `cargo build`/`cargo run`: build.rs copies assets/ into
# target/ at build time.

set -euo pipefail

force=0
[[ "${1:-}" == "--force" ]] && force=1

if ! command -v inkscape >/dev/null; then
    echo "inkscape not found - install it to convert the SVG art" >&2
    exit 1
fi

cd "$(dirname "$0")"

count=0
while IFS= read -r -d '' svg; do
    png="${svg%.svg}.png"
    if [[ $force -eq 0 && -f "$png" && "$png" -nt "$svg" ]]; then
        continue
    fi
    echo "$svg -> $png"
    inkscape --export-type=png --export-filename="$png" "$svg" >/dev/null 2>&1
    count=$((count + 1))
done < <(find . -name '*.svg' -print0)

echo "converted $count file(s)"
