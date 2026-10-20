#!/bin/zsh
# Builds a signed bb1.app and zips it.
#
#   scripts/release.sh             test build:  dist/bb1-<VERSION>-build<N>.zip  (N = commit count)
#   scripts/release.sh --publish   test build, published as a GitHub pre-release (tag build-<N>);
#                                  the app offers it as an update when "Include test builds" is on
#   scripts/release.sh --release   real release: dist/bb1-v<VERSION>-build<N>.zip, tag v<VERSION>,
#                                  published as a GitHub release
#
# The app finds updates by the zip's name on github.com/spheceo/bb1 releases, so keep the
# "bb1-…-build<N>.zip" pattern. Publishing pushes the current branch first (a release needs its commit on GitHub).
#
# Versioning: v0.0.0 was the first release. Everything after it is a test build (--publish) until the
# owner says to make a proper release; then bump the last number in VERSION (0.0.0 → 0.0.1 → …),
# add a "## v<VERSION>" section to CHANGELOG.md, and run --release.
set -euo pipefail
root=${0:A:h:h}
cd "$root"

version=$(cat VERSION)
build=$(git rev-list --count HEAD 2>/dev/null || echo 0)
mode=${1:-test}

if [[ "$mode" == "--release" || "$mode" == "--publish" ]]; then
  [[ -z "$(git status --porcelain)" ]] || { echo "!! Uncommitted changes; commit first."; exit 1; }
fi
if [[ "$mode" == "--release" ]]; then
  grep -q "^## v$version" CHANGELOG.md || { echo "!! CHANGELOG.md has no '## v$version' entry"; exit 1; }
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "!! v$version is already released"; exit 1; }
  tag="v$version"
elif [[ "$mode" == "--publish" ]]; then
  tag="build-$build"
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null && { echo "!! $tag is already published"; exit 1; }
fi

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

if [[ "$mode" == "--release" || "$mode" == "--publish" ]]; then
  # What changed since the last published build.
  previous=$(git tag --list 'build-*' 'v*' --sort=-creatordate | head -1)
  notes=$(git log --no-merges --format='- %s' ${previous:+$previous..}HEAD)
  branch=$(git rev-parse --abbrev-ref HEAD)
  git push -q origin "$branch"
  git tag "$tag"
  git push -q origin "$tag"
  if [[ "$mode" == "--release" ]]; then
    gh release create "$tag" "$zip" --title "bb1 $version" --notes "$notes" --latest
  else
    gh release create "$tag" "$zip" --title "Test build $build" --prerelease --notes "Test build $build (version $version).

$notes"
  fi
  echo "==> published $tag"
fi
