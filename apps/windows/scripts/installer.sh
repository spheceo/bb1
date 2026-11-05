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
# makensis crashes (bad_alloc) without a UTF-8 locale.
LC_ALL=en_US.UTF-8 makensis -V2 -DVERSION="$version" -DBUILD="$build" \
  -DEXE="$PWD/target/$target/release/bb1.exe" -DOUT="$PWD/$out" installer/bb1.nsi
echo "$out"
