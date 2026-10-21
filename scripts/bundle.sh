#!/bin/zsh
# Build the signed app: scripts/bundle.sh  →  build/bb1.app
# One app for both roles; the mode is chosen in the app (or `bb1 host` / `bb1 client`).
#
# Signs with a local self-signed certificate (scripts/signing-identity.sh) so macOS keeps
# Screen Recording / Accessibility permissions across rebuilds. Override with
# BB1_SIGN_IDENTITY="<name or SHA-1>".
set -euo pipefail

root=${0:A:h:h}
cd "$root"

echo "==> Building bb1 (release)"
swift build -c release --product bb1

app="build/bb1.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp ".build/release/bb1" "$app/Contents/MacOS/bb1"
cp "Resources/Info.plist" "$app/Contents/Info.plist"
version=$(cat VERSION)
build=$(git rev-list --count HEAD 2>/dev/null || echo 1)
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" -c "Set :CFBundleVersion $build" "$app/Contents/Info.plist"
# A proper release says so (the app shows "bb1 v0.0.1" instead of "bb1 nightly-N").
[[ -n "${BB1_RELEASE:-}" ]] && /usr/libexec/PlistBuddy -c "Add :BB1Release bool true" "$app/Contents/Info.plist"
cp "Resources/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"   # made by scripts/make-icon.swift

# Developer ID (Apple-issued) when it's installed: hardened runtime and a secure timestamp, which
# notarization needs. Otherwise the local certificate. BB1_SIGN_IDENTITY overrides both.
local_identity=$(scripts/signing-identity.sh)
developer_id=$(scripts/signing-identity.sh --developer-id)
identity=${BB1_SIGN_IDENTITY:-${developer_id:-$local_identity}}
if [[ -z "$identity" ]]; then
  echo "!! Couldn't set up a signing certificate; signing ad-hoc."
  echo "!! macOS will forget this app's permissions every time you rebuild."
  identity="-"
fi
echo "==> Signing with $identity"
if [[ "$identity" == "Developer ID Application:"* ]]; then
  codesign --force --sign "$identity" --options runtime --timestamp "$app"
else
  codesign --force --sign "$identity" --timestamp=none "$app"
fi
codesign --verify --verbose=1 "$app"
echo "==> $app"
