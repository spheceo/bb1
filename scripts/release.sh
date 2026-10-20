#!/bin/zsh
# Builds a signed bb1.app and zips it.
#
#   scripts/release.sh             test build:  dist/bb1-<VERSION>-build<N>.zip  (N = commit count)
#   scripts/release.sh --release   real release: dist/bb1-v<VERSION>.zip + git tag v<VERSION>
#
# Versioning: VERSION stays frozen (now 0.0.1) until the owner says "make the first release".
# Only the last number ever changes (0.0.1 → 0.0.2 → …), and only for a release the owner asks for.
set -euo pipefail
root=${0:A:h:h}
cd "$root"

version=$(cat VERSION)
build=$(git rev-list --count HEAD 2>/dev/null || echo 0)
mode=${1:-test}

if [[ "$mode" == "--release" ]]; then
  grep -q "^## v$version" CHANGELOG.md || { echo "!! CHANGELOG.md has no '## v$version' entry"; exit 1; }
  [[ -z "$(git status --porcelain)" ]] || { echo "!! Uncommitted changes; commit first."; exit 1; }
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "!! v$version is already released"; exit 1; }
fi

scripts/bundle.sh
mkdir -p dist
if [[ "$mode" == "--release" ]]; then
  zip="dist/bb1-v$version.zip"
else
  dirty=$([[ -n "$(git status --porcelain 2>/dev/null)" ]] && echo "-modified" || echo "")
  zip="dist/bb1-$version-build$build$dirty.zip"
fi
rm -f "$zip"
ditto -c -k --keepParent build/bb1.app "$zip"
echo "==> $zip ($(du -h "$zip" | cut -f1))"
if [[ "$mode" == "--release" ]]; then
  git tag "v$version"
  echo "==> tagged v$version"
fi
