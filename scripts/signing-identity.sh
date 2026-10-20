#!/bin/zsh
# Creates (once) a self-signed code-signing certificate just for bb1, in its own keychain:
#   ~/Library/Application Support/bb1/signing/bb1-signing.keychain-db
# Prints the certificate's SHA-1 (for codesign -s) on success. Used by bundle.sh.
# The keychain is added to your keychain search list so codesign can find it.
#
# Why: macOS ties Screen Recording / Accessibility permissions to the signing certificate.
# A local certificate stays the same across rebuilds and can't be revoked by Apple.
# Undo: remove it from Keychain Access's list (or `security list-keychains -d user -s ~/Library/Keychains/login.keychain-db`)
# and delete ~/Library/Application Support/bb1/signing.
set -euo pipefail
# Apple's LibreSSL on purpose: Homebrew OpenSSL 3+ writes .p12 files the keychain can't read.

name="bb1 Local Signing"
dir="$HOME/Library/Application Support/bb1/signing"
kc="$dir/bb1-signing.keychain-db"
pwfile="$dir/keychain-password"

mkdir -p "$dir"
chmod 700 "$dir"
if [[ ! -f "$pwfile" ]]; then
  LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 32 >"$pwfile"
  chmod 600 "$pwfile"
fi
pw=$(cat "$pwfile")

if [[ ! -f "$kc" ]]; then
  security create-keychain -p "$pw" "$kc" >/dev/null
  security set-keychain-settings "$kc" # never auto-lock
fi
security unlock-keychain -p "$pw" "$kc"

if ! security find-certificate -c "$name" "$kc" >/dev/null 2>&1; then
  tmp=$(mktemp -d)
  cat >"$tmp/cfg" <<EOF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$tmp/key.pem" -out "$tmp/cert.pem" \
    -config "$tmp/cfg" -extensions ext 2>/dev/null
  /usr/bin/openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -name "$name" \
    -out "$tmp/id.p12" -passout "pass:$pw" 2>/dev/null
  security import "$tmp/id.p12" -k "$kc" -P "$pw" -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$kc" >/dev/null
  rm -rf "$tmp"
fi

# codesign only finds identities in keychains on the search list.
if ! security list-keychains -d user | grep -qF "$kc"; then
  current=("${(@f)$(security list-keychains -d user | sed -e 's/^ *"//' -e 's/"$//')}")
  security list-keychains -d user -s "${current[@]}" "$kc"
fi

security find-identity -p codesigning "$kc" | grep "\"$name\"" | head -1 | awk '{print $2}'
