#!/bin/bash
# Expose the release only after both verified deliverables have uploaded successfully.
set -euo pipefail
tag=${BB1_RELEASE_TAG:?}
version=$(cat VERSION)
build=$(git rev-list --count HEAD)
nightly=$((build - 25))
args=()
case "$tag" in
  "nightly-$nightly")
    title="bb1 nightly-$nightly"
    dmg="dist/bb1-nightly-$nightly.dmg"
    zip="dist/bb1-$version-build$build.zip"
    args+=(--prerelease)
    intro="bb1 nightly-$nightly (build $build). Installed copies on the Nightly channel update themselves."
    ;;
  "v$version")
    title="bb1 v$version"
    dmg="dist/bb1-v$version.dmg"
    zip="dist/bb1-v$version-build$build.zip"
    intro='Download the .dmg, open it and drag bb1 into Applications. Installed copies update themselves.'
    ;;
  *) echo "Invalid release tag $tag" >&2; exit 1 ;;
esac
[[ -s "$dmg" && -s "$zip" && -s dist/SHA256SUMS ]] || { echo 'Missing release deliverables.' >&2; exit 1; }
(cd dist && shasum -a 256 -c SHA256SUMS)
if gh release view "$tag" --json isDraft >/dev/null 2>&1; then
  [[ $(gh release view "$tag" --json isDraft --jq .isDraft) == true ]] || { echo 'Refusing to replace an already published release.' >&2; exit 1; }
else
  previous=$(git tag --list 'nightly-*' 'build-*' 'v*' --sort=-creatordate --merged HEAD^ | sed -n '1p')
  notes=$(mktemp)
  trap 'rm -f "$notes"' EXIT
  {
    printf '%s\n\n' "$intro"
    git log --no-merges --format='- %s' "${previous:+$previous..}HEAD"
  } > "$notes"
  gh release create "$tag" --verify-tag --draft "${args[@]}" --title "$title" --notes-file "$notes"
fi
gh release upload "$tag" --clobber "$dmg" "$zip" dist/SHA256SUMS
if [[ "$tag" == nightly-* ]]; then
  gh release edit "$tag" --draft=false --prerelease --latest=false
else
  gh release edit "$tag" --draft=false --prerelease=false --latest
fi
