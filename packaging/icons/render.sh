#!/bin/sh
# Regenerates every platform icon from packaging/icons/tabula.svg. Run on
# macOS (needs Google Chrome to render the SVG, ImageMagick, and iconutil):
#   packaging/icons/render.sh
# The outputs are committed, so CI doesn't need any of these tools.
set -eu
cd "$(dirname "$0")/../.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Chrome renders the SVG (gradients, shadow) better than ImageMagick's
# built-in renderer.
printf '<html><body style="margin:0">%s</body></html>' "$(cat packaging/icons/tabula.svg)" > "$tmp/icon.html"
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu \
  --hide-scrollbars --default-background-color=00000000 --window-size=1024,1024 \
  --screenshot="$tmp/1024.png" "file://$tmp/icon.html" 2>/dev/null
png() { magick "$tmp/1024.png" -resize "$1x$1" "$2"; }

# macOS: Tabula.icns for the app bundle.
set_dir="$tmp/Tabula.iconset"
mkdir "$set_dir"
for s in 16 32 128 256 512; do
  png "$s" "$set_dir/icon_${s}x${s}.png"
  png $((s * 2)) "$set_dir/icon_${s}x${s}@2x.png"
done
iconutil -c icns -o packaging/macos/Tabula.icns "$set_dir"

# Windows: tabula.ico, embedded in the .exe by build.rs.
magick "$tmp/1024.png" -define icon:auto-resize=256,64,48,32,24,16 packaging/windows/tabula.ico

# Linux: a 512px PNG for the hicolor theme (the .deb also installs the SVG).
png 512 packaging/linux/tabula.png
