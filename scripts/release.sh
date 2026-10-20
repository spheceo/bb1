#!/bin/zsh
# Build a release: signed bb1.app → dist/bb1-v<VERSION>.zip
#
# Versions: only the last number changes (0.0.1 → 0.0.2 → 0.0.3 …). The first two stay as they are
# unless the owner explicitly says otherwise. Bump VERSION, add a CHANGELOG.md entry, commit, then:
#   scripts/release.sh            # build the zip
#   scripts/release.sh --tag      # …and tag the commit v<VERSION>
set -euo pipefail
root=${0:A:h:h}
cd "$root"

version=$(cat VERSION)
grep -q "^## v$version" CHANGELOG.md || { echo "!! CHANGELOG.md has no '## v$version' entry"; exit 1; }
if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
  echo "!! Uncommitted changes; commit first so the release matches a commit."
  exit 1
fi

scripts/bundle.sh
mkdir -p dist
zip="dist/bb1-v$version.zip"
rm -f "$zip"
ditto -c -k --keepParent build/bb1.app "$zip"
echo "==> $zip ($(du -h "$zip" | cut -f1))"

if [[ "${1:-}" == "--tag" ]]; then
  git tag -f "v$version"
  echo "==> tagged v$version"
fi
