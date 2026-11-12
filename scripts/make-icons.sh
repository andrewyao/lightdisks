#!/usr/bin/env bash

# Render assets/icon/lightdisks.svg into AppIcon.icns, which make-dmg.sh copies
# into LightDisks.app. The output is committed, so only re-run this after
# editing the SVG.
#
# Needs resvg (`cargo install resvg`) and macOS's iconutil.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/assets/icon"
SVG="$DIR/lightdisks.svg"

command -v resvg >/dev/null || { echo "resvg not found: cargo install resvg" >&2; exit 1; }

ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  resvg -w "$size" -h "$size" "$SVG" "$ICONSET/icon_${size}x${size}.png"
  resvg -w "$((size * 2))" -h "$((size * 2))" "$SVG" "$ICONSET/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$DIR/AppIcon.icns"

echo "==> Wrote $DIR/AppIcon.icns"
