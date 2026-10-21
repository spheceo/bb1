#!/bin/zsh
# Wraps build/bb1.app in a drag-to-Applications disk image.
#   scripts/make-dmg.sh <output.dmg>
# Uses dmgbuild (lays out the window without scripting Finder), installed once into its own
# Python environment in ~/Library/Caches/bb1/dmgbuild.
# The disk is named " " so Finder's title bar shows no text over the window (just the disk's bb1 icon,
# which Finder always shows); the window itself says what to do.
set -euo pipefail
root=${0:A:h:h}
cd "$root"
out=${1:?usage: make-dmg.sh <output.dmg>}

venv="$HOME/Library/Caches/bb1/dmgbuild"
if [[ ! -x "$venv/bin/dmgbuild" ]]; then
  echo "==> Installing dmgbuild (once)"
  python3 -m venv "$venv"
  "$venv/bin/pip" install -q dmgbuild
fi

work="build/dmg"
rm -rf "$work"
mkdir -p "$work"
swift scripts/make-dmg-background.swift "$work" >/dev/null
tiffutil -cathidpicheck "$work/background.png" "$work/background@2x.png" -out "$work/background.tiff" 2>/dev/null

rm -f "$out"
"$venv/bin/dmgbuild" -s scripts/dmg-settings.py \
  -D app="build/bb1.app" -D background="$work/background.tiff" -D icon="Resources/AppIcon.icns" \
  " " "$out" >/dev/null
echo "==> $out ($(du -h "$out" | cut -f1))"
