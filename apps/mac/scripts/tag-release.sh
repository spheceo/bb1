#!/bin/zsh
# Queue a build on GitHub. Full history is required: installed apps compare commit counts.
set -euo pipefail
cd "${0:A:h:h}"
mode=${1:?usage: tag-release.sh nightly|stable}
[[ "$mode" == nightly || "$mode" == stable ]] || exit 2
[[ $(git rev-parse --is-shallow-repository) == false ]] || { echo 'Full git history is required.' >&2; exit 1; }
[[ $(git branch --show-current) == main ]] || { echo 'Publish from main.' >&2; exit 1; }
[[ -z $(git status --porcelain) ]] || { echo 'Commit changes before publishing.' >&2; exit 1; }
version=$(cat VERSION)
build=$(git rev-list --count HEAD)
if [[ "$mode" == stable ]]; then
  grep -q "^## v$version" CHANGELOG.md || { echo "Missing changelog entry for v$version." >&2; exit 1; }
  tag="v$version"
else
  tag="nightly-$((build - 25))"
fi
git show-ref --verify --quiet "refs/tags/$tag" && { echo "$tag already exists." >&2; exit 1; }
[[ -z $(git ls-remote --tags origin "refs/tags/$tag") ]] || { echo "$tag already exists on GitHub." >&2; exit 1; }
git push origin main
git tag "$tag"
git push origin "refs/tags/$tag"
echo "==> Queued $tag on GitHub Actions. No local build is needed."
echo '    https://github.com/spheceo/bb1/actions/workflows/release.yml'
