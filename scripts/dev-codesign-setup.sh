#!/bin/bash
# One-time setup: creates a local self-signed code-signing certificate so
# `dev-codesign-runner.sh` can sign dev builds with a *stable* identity
# instead of ad-hoc (`codesign --sign -`).
#
# Why this is needed: an ad-hoc signature has no certificate to anchor to,
# so codesign auto-generates a designated requirement pinned to the
# binary's exact content hash (cdhash). Every `cargo build` produces a
# different binary, so the cdhash changes on every rebuild — the macOS
# Keychain ACL behind "Always Allow" for the OpenPal master-key entry
# (crates/core/src/crypto/keychain.rs) never matches the new build, so it
# re-prompts on every `npm run dev`. Signing with a real certificate (even
# a self-signed one) gives codesign a stable anchor — the certificate
# itself — so the requirement stays satisfied across rebuilds and the
# "Always Allow" grant sticks after the first prompt.
#
# Run this once per machine: ./scripts/dev-codesign-setup.sh
set -euo pipefail

CERT_NAME="OpenPal Dev Code Signing"
LOGIN_KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$CERT_NAME" login.keychain-db >/dev/null 2>&1; then
  echo "✓ '$CERT_NAME' already exists in your login keychain — nothing to do."
  exit 0
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
chmod 700 "$workdir"

echo "Generating a local self-signed code-signing certificate ('$CERT_NAME')..."
openssl req -x509 -newkey rsa:2048 -keyout "$workdir/key.pem" -out "$workdir/cert.pem" \
  -days 3650 -nodes -subj "/CN=$CERT_NAME" \
  -addext "extendedKeyUsage=codeSigning" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" >/dev/null 2>&1

# -legacy: macOS's Security framework can't parse the AES/SHA-256 PKCS12
# encoding OpenSSL 3.x produces by default — it needs the older RC2/3DES +
# SHA1 encoding ("MAC verification failed during PKCS12 import" otherwise).
# A non-empty transient password is also required: macOS's importer fails
# MAC verification on an empty-string PKCS12 password even with -legacy.
# The password never leaves this machine and the file is deleted on exit.
transient_pw="$(openssl rand -base64 24)"
openssl pkcs12 -export -legacy -inkey "$workdir/key.pem" -in "$workdir/cert.pem" \
  -out "$workdir/cert.p12" -passout "pass:$transient_pw"

echo "Importing into your login keychain (macOS may ask you to confirm access for codesign)..."
security import "$workdir/cert.p12" -k "$LOGIN_KEYCHAIN" -T /usr/bin/codesign -P "$transient_pw"

echo "Granting codesign standing access to the new key (best-effort — needs an interactive"
echo "terminal/Touch ID prompt, so this step is skipped if run non-interactively):"
if ! security set-key-partition-list -S apple-tool:,apple: -s "$LOGIN_KEYCHAIN" </dev/tty >/dev/tty 2>&1; then
  echo "  (skipped/failed — not fatal: codesign will just show one native 'Always Allow'" >&2
  echo "  dialog the first time it signs; click Allow and it will stick from then on.)" >&2
fi

echo
echo "✓ Done. '$CERT_NAME' is ready — dev-codesign-runner.sh will pick it up automatically."
echo "  The next 'npm run dev' will still prompt once (maybe twice) to grant access to"
echo "  codesign and/or the master-key keychain entry; click 'Always Allow' each time and"
echo "  it should not prompt again across rebuilds."
