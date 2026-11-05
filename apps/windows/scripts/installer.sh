#!/usr/bin/env bash
# Build bb1.exe for Windows and wrap it in bb1-setup.exe, on a Mac.
# Once: rustup target add x86_64-pc-windows-gnu && brew install mingw-w64 makensis
set -euo pipefail
cd "$(dirname "$0")/.."
target=x86_64-pc-windows-gnu
cargo build --release --locked --target $target --bin bb1
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
build=$(git rev-list --count HEAD)
out=target/$target/release/bb1-setup.exe

# SDL3.dll (game controllers) from SDL's official release, checked, cached in target/.
# Same version and checksum as .github/workflows/windows.yml.
sdl_version=3.2.30
sdl_sha256=8b9986c87751fc976cf6d606fafb614ec683d8b23bf651cb3027cc9016b25bb4
sdl_dir=target/sdl3-$sdl_version
if [[ ! -f $sdl_dir/SDL3.dll ]]; then
  mkdir -p "$sdl_dir"
  zip=$sdl_dir/SDL3-$sdl_version-win32-x64.zip
  curl -fsSL -o "$zip" "https://github.com/libsdl-org/SDL/releases/download/release-$sdl_version/SDL3-$sdl_version-win32-x64.zip"
  echo "$sdl_sha256  $zip" | shasum -a 256 -c - >/dev/null || { echo "SDL3 zip checksum mismatch" >&2; rm -f "$zip"; exit 1; }
  unzip -o -q "$zip" SDL3.dll LICENSE.txt -d "$sdl_dir"
fi

# makensis crashes (bad_alloc) without a UTF-8 locale.
LC_ALL=en_US.UTF-8 makensis -V2 -DVERSION="$version" -DBUILD="$build" \
  -DEXE="$PWD/target/$target/release/bb1.exe" -DSDL="$PWD/$sdl_dir/SDL3.dll" -DOUT="$PWD/$out" installer/bb1.nsi
echo "$out"
