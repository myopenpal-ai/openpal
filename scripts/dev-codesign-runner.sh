#!/bin/sh
# Cargo runner (see ../.cargo/config.toml): re-signs the freshly built
# binary with a fixed --identifier before running it, so the OS keychain's
# "Always Allow" grant survives across dev rebuilds. Only registered for
# macOS targets.
#
# Signs with the local "OpenPal Dev Code Signing" certificate (see
# dev-codesign-setup.sh) when it's been set up, since ad-hoc signing
# (`--sign -`) has no certificate to anchor to: codesign auto-generates a
# designated requirement pinned to the binary's exact content hash, which
# changes on every rebuild and defeats "Always Allow" regardless of
# --identifier. Falls back to ad-hoc if the cert hasn't been created yet,
# so a fresh checkout still builds and runs — just with the original
# every-launch keychain prompt until setup is run.
set -e
CERT_NAME="OpenPal Dev Code Signing"
if security find-certificate -c "$CERT_NAME" login.keychain-db >/dev/null 2>&1; then
  IDENTITY="$CERT_NAME"
else
  echo "dev-codesign-runner: '$CERT_NAME' not found — signing ad-hoc." >&2
  echo "dev-codesign-runner: run ./scripts/dev-codesign-setup.sh once to stop the repeat keychain prompts." >&2
  IDENTITY="-"
fi
codesign --force --sign "$IDENTITY" --identifier com.myopenpal.openpal.dev "$1"
exec "$@"
