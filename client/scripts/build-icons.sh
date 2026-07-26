#!/usr/bin/env bash
# Rasterize the PWA app icons from icons/icon.svg into public/ (#15).
#
# Run by hand when the mark changes; the PNGs are committed, so neither the
# build nor CI depends on this. It uses macOS's `sips` (ImageIO renders SVG),
# which is why it is a one-off rather than a build step — dev happens on a Mac,
# the Pi only ever serves the results.
#
#   client/scripts/build-icons.sh
set -euo pipefail

cd "$(dirname "$0")/.."
source="icons/icon.svg"

render() { # <size> <output>
  sips -s format png -Z "$1" "$source" --out "public/$2" >/dev/null
  printf '  public/%-24s %sx%s\n' "$2" "$1" "$1"
}

echo "rendering $source"
# The two sizes Chrome's install criteria want, at `purpose: any maskable`.
render 192 icon-192.png
render 512 icon-512.png
# iOS ignores the manifest's icons for the home screen and reads this instead.
render 180 apple-touch-icon.png
