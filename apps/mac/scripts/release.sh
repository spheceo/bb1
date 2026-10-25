#!/bin/zsh
# Local development: scripts/release.sh builds into dist/ without publishing/notarizing.
# Publish: --publish (nightly) or --release (stable, only when requested) pushes a tag;
# GitHub Actions then builds, signs, notarizes and publishes. --ci runs that packaging job.
set -euo pipefail
cd "${0:A:h:h}"
mode=${1:-test}
case "$mode" in
  --publish) exec scripts/tag-release.sh nightly ;;
  --release) exec scripts/tag-release.sh stable ;;
  test|--ci) ;;
  *) echo 'usage: scripts/release.sh [--publish|--release|--ci]' >&2; exit 2 ;;
esac

version=$(cat VERSION)
build=$(git rev-list --count HEAD)
nightly=$((build - 25)) # BB1.nightlyBase; installed updaters depend on these names and counts.
if [[ "$mode" == --ci ]]; then
  [[ "${GITHUB_ACTIONS:-}" == true && -n "${BB1_SIGN_IDENTITY:-}" ]] || { echo 'CI signing is not configured.' >&2; exit 1; }
  [[ $(git rev-parse --is-shallow-repository) == false ]] || { echo 'Full git history is required.' >&2; exit 1; }
  case "${BB1_RELEASE_TAG:-}" in
    '') ;;
    "nightly-$nightly") ;;
    "v$version")
      grep -q "^## v$version" CHANGELOG.md || { echo 'Missing stable changelog entry.' >&2; exit 1; }
      export BB1_RELEASE=1
      ;;
    *) echo "Tag ${BB1_RELEASE_TAG} doesn't match this commit's build/version." >&2; exit 1 ;;
  esac
  if [[ -n "${BB1_RELEASE_TAG:-}" && "${BB1_NOTARIZE:-}" != 1 ]]; then
    echo 'Published builds must be notarized.' >&2; exit 1
  fi
fi

scripts/bundle.sh
mkdir -p dist build/notary
signature=$(codesign -dvv build/bb1.app 2>&1)
signed_by=$(awk -F= '/^Authority=Developer ID Application/ {print $2; exit}' <<< "$signature")
notarize=${BB1_NOTARIZE:-0}
if [[ "$mode" == --ci && -z "$signed_by" ]]; then
  echo 'CI requires a Developer ID signature.' >&2; exit 1
fi
notary_args=(--keychain-profile bb1-notary)
if [[ -n "${BB1_NOTARY_KEY_PATH:-}" ]]; then
  notary_args=(--key "$BB1_NOTARY_KEY_PATH" --key-id "${BB1_NOTARY_KEY_ID:?}" --issuer "${BB1_NOTARY_ISSUER:?}")
fi
notarize_file() {
  local file=$1 label=$2 receipt="build/notary/$2.json" id result
  echo "==> Submitting $file to Apple"
  # Save the submission ID before waiting, so a timeout never loses Apple's receipt.
  xcrun notarytool submit "$file" "${notary_args[@]}" --output-format json > "$receipt"
  id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$receipt")
  echo "==> Apple submission $id ($label)"
  if ! xcrun notarytool wait "$id" "${notary_args[@]}" --timeout 150m --output-format json > "build/notary/$label-result.json"; then
    cat "build/notary/$label-result.json"
    echo "Apple submission $id did not finish successfully; see the saved receipts." >&2
    return 1
  fi
  result=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["status"])' "build/notary/$label-result.json")
  if [[ "$result" != Accepted ]]; then
    xcrun notarytool log "$id" "${notary_args[@]}" "build/notary/$label-log.json" || true
    echo "Apple rejected $label ($id): $result" >&2
    return 1
  fi
}
if [[ "$notarize" == 1 ]]; then
  [[ -n "$signed_by" ]] || { echo 'Notarization requires Developer ID.' >&2; exit 1; }
  ditto -c -k --keepParent build/bb1.app build/notarize.zip
  notarize_file build/notarize.zip app
  xcrun stapler staple -q build/bb1.app
  xcrun stapler validate build/bb1.app
fi

if [[ "${BB1_RELEASE:-}" == 1 ]]; then
  zip="dist/bb1-v$version-build$build.zip"
  dmg="dist/bb1-v$version.dmg"
else
  dirty=''
  [[ -n $(git status --porcelain) && "$mode" != --ci ]] && dirty='-modified'
  zip="dist/bb1-$version-build$build$dirty.zip"
  dmg="dist/bb1-nightly-$nightly$dirty.dmg"
fi
rm -f "$zip"
ditto -c -k --keepParent build/bb1.app "$zip"
check=$(mktemp -d)
mounted=0
cleanup() {
  if [[ "$mounted" == 1 ]]; then hdiutil detach "$check/dmg" >/dev/null || true; fi
  rm -rf "$check"
}
trap cleanup EXIT
scripts/make-dmg.sh "$dmg"
if [[ -n "$signed_by" ]]; then
  keychain_args=()
  [[ -n "${BB1_SIGN_KEYCHAIN:-}" ]] && keychain_args=(--keychain "$BB1_SIGN_KEYCHAIN")
  codesign --force --sign "$signed_by" "${keychain_args[@]}" --timestamp "$dmg"
  codesign --verify --strict "$dmg"
  # Match the Developer ID requirement accepted by the nightly-5 bridge updater.
  codesign --verify --strict -R='anchor apple generic and identifier "com.bb1.host" and certificate leaf[subject.OU] = "5PLDN9BDV4" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists' build/bb1.app
fi
# The layout tool must not mutate the app's signature while placing it in the image.
mkdir "$check/dmg"
hdiutil attach -readonly -nobrowse -mountpoint "$check/dmg" "$dmg" >/dev/null
mounted=1
codesign --verify --strict "$check/dmg/bb1.app"
[[ -L "$check/dmg/ " && $(readlink "$check/dmg/ ") == /Applications ]]
[[ -f "$check/dmg/.background.tiff" ]]
if [[ "$notarize" == 1 ]]; then xcrun stapler validate "$check/dmg/bb1.app"; fi
hdiutil detach "$check/dmg" >/dev/null
mounted=0
if [[ "$notarize" == 1 ]]; then
  notarize_file "$dmg" dmg
  xcrun stapler staple -q "$dmg"
  xcrun stapler validate "$dmg"
  spctl --assess --type execute --verbose=2 build/bb1.app
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi
# Verify the actual update archive, including the top-level name and stamped build.
ditto -x -k "$zip" "$check"
codesign --verify --strict "$check/bb1.app"
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$check/bb1.app/Contents/Info.plist") == com.bb1.host ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$check/bb1.app/Contents/Info.plist") == "$build" ]]
if [[ "$notarize" == 1 ]]; then
  xcrun stapler validate "$check/bb1.app"
fi
(cd dist && shasum -a 256 "${dmg:t}" "${zip:t}" > SHA256SUMS)
echo "==> Verified $zip and $dmg (build $build)"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### bb1 build $build"
    echo "- Disk image: ${dmg:t}"
    echo "- Update archive: ${zip:t}"
    echo "- Signing team: 5PLDN9BDV4"
    echo "- Notarized and stapled: $notarize"
    echo "- Commit: $(git rev-parse HEAD)"
  } >> "$GITHUB_STEP_SUMMARY"
fi
