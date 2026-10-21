#!/bin/zsh
# Builds a signed bb1.app, a drag-to-Applications disk image (.dmg, for installing by hand) and a
# zip (what the in-app updater downloads).
#
#   scripts/release.sh             nightly, local only: dist/bb1-<VERSION>-build<N>.zip and bb1-nightly-<M>.dmg
#   scripts/release.sh --publish   nightly, published as a GitHub pre-release "bb1 nightly-<M>" (tag nightly-<M>);
#                                  the app offers it on the Nightly channel
#   scripts/release.sh --release   real release: dist/bb1-v<VERSION>-build<N>.zip and bb1-v<VERSION>.dmg,
#                                  tag v<VERSION>, published as a GitHub release (the Stable channel)
#
# N = build number (commit count); updates compare it. M = nightly number = N - 25 (BB1.nightlyBase), so
# build 26 was nightly-1. The app finds updates by the zip's name on github.com/spheceo/bb1 releases, so
# keep the "bb1-…-build<N>.zip" pattern (installed copies parse it). Publishing pushes the current branch first.
#
# Versioning: v0.0.0 was the first release. Everything after it is a nightly (--publish) until the
# owner says to make a proper release; then bump the last number in VERSION (0.0.0 → 0.0.1 → …),
# add a "## v<VERSION>" section to CHANGELOG.md, and run --release.
set -euo pipefail
root=${0:A:h:h}
cd "$root"

version=$(cat VERSION)
build=$(git rev-list --count HEAD 2>/dev/null || echo 0)
mode=${1:-test}
nightly=$((build - 25)) # keep in step with BB1.nightlyBase

if [[ "$mode" == "--release" || "$mode" == "--publish" ]]; then
  [[ -z "$(git status --porcelain)" ]] || { echo "!! Uncommitted changes; commit first."; exit 1; }
fi
if [[ "$mode" == "--release" ]]; then
  grep -q "^## v$version" CHANGELOG.md || { echo "!! CHANGELOG.md has no '## v$version' entry"; exit 1; }
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "!! v$version is already released"; exit 1; }
  tag="v$version"
elif [[ "$mode" == "--publish" ]]; then
  tag="nightly-$nightly"
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null && { echo "!! $tag is already published"; exit 1; }
fi

[[ "$mode" == "--release" ]] && export BB1_RELEASE=1
scripts/bundle.sh
mkdir -p dist
if [[ "$mode" == "--release" ]]; then
  zip="dist/bb1-v$version-build$build.zip"
else
  dirty=$([[ -n "$(git status --porcelain 2>/dev/null)" ]] && echo "-modified" || echo "")
  zip="dist/bb1-$version-build$build$dirty.zip"
fi
rm -f "$zip"
ditto -c -k --keepParent build/bb1.app "$zip"
echo "==> $zip ($(du -h "$zip" | cut -f1))"
if [[ "$mode" == "--release" ]]; then dmg="dist/bb1-v$version.dmg"; else dmg="dist/bb1-nightly-$nightly${dirty:-}.dmg"; fi
scripts/make-dmg.sh "$dmg"

if [[ "$mode" == "--release" || "$mode" == "--publish" ]]; then
  # What changed since the last published build.
  previous=$(git tag --list 'nightly-*' 'build-*' 'v*' --sort=-creatordate | head -1)
  notes=$(git log --no-merges --format='- %s' ${previous:+$previous..}HEAD)
  branch=$(git rev-parse --abbrev-ref HEAD)
  git push -q origin "$branch"
  git tag "$tag"
  git push -q origin "$tag"
  if [[ "$mode" == "--release" ]]; then
    gh release create "$tag" "$dmg" "$zip" --title "bb1 $version" --latest --notes "Download the .dmg, open it and drag bb1 into Applications. Installed copies update themselves.

$notes"
  else
    gh release create "$tag" "$dmg" "$zip" --title "bb1 nightly-$nightly" --prerelease --notes "bb1 nightly-$nightly (build $build). Installed copies on the Nightly channel update themselves.

$notes"
  fi
  echo "==> published $tag"
fi
