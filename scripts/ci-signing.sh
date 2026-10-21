#!/bin/bash
# Import secrets into a disposable runner keychain. Never use this on the owner's login keychain.
set -euo pipefail
[[ "${GITHUB_ACTIONS:-}" == true && -n "${RUNNER_TEMP:-}" && -n "${GITHUB_ENV:-}" ]] || exit 1
for name in MACOS_CERTIFICATE MACOS_CERTIFICATE_PASSWORD NOTARY_KEY NOTARY_KEY_ID NOTARY_ISSUER; do
  [[ -n "${!name:-}" ]] || { echo "::error::Missing $name secret"; exit 1; }
done
umask 077
work="$RUNNER_TEMP/bb1-signing"
mkdir -p "$work"
keychain="$work/signing.keychain-db"
password=$(openssl rand -hex 24)
echo "::add-mask::$password"
printf %s "$MACOS_CERTIFICATE" | base64 --decode > "$work/certificate.p12"
printf %s "$NOTARY_KEY" | base64 --decode > "$work/notary.p8"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$work/certificate.p12" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
# This is a disposable GitHub VM. The local signing script is deliberately never called here.
security list-keychains -d user -s "$keychain" "$HOME/Library/Keychains/login.keychain-db"
identity=$(security find-identity -v -p codesigning "$keychain" | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)
[[ "$identity" == *'(5PLDN9BDV4)' ]] || { echo '::error::Expected bb1 Developer ID for team 5PLDN9BDV4'; exit 1; }
{
  echo "BB1_SIGN_IDENTITY=$identity"
  echo "BB1_SIGN_KEYCHAIN=$keychain"
  echo "BB1_NOTARY_KEY_PATH=$work/notary.p8"
  echo "BB1_NOTARY_KEY_ID=$NOTARY_KEY_ID"
  echo "BB1_NOTARY_ISSUER=$NOTARY_ISSUER"
} >> "$GITHUB_ENV"
rm "$work/certificate.p12"
