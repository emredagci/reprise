#!/usr/bin/env bash
# Builds the sbrain CLI for TARGET and packs it as dist/sbrain-<version>-<platform>.tar.gz (macos-arm64,
# macos-x86_64, linux-arm64 or linux-x86_64): one folder holding
# `sbrain`, LICENSE, NOTICE and README.md. The release workflow runs it once per target; it also runs by hand.
#
#   TARGET=aarch64-apple-darwin ./.github/package.sh
#
# On macOS, with SIGN_IDENTITY set (a "Developer ID Application" identity in the keychain), the binary is
# signed with the hardened runtime and a timestamp. With NOTARY_KEY_PATH, NOTARY_KEY_ID and NOTARY_ISSUER
# set as well (an App Store Connect API key), Apple notarizes it; a lone binary cannot be stapled, so
# Gatekeeper looks the ticket up online. Without them the binary is packed unsigned.
set -euo pipefail

: "${TARGET:?set TARGET, e.g. aarch64-apple-darwin}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
test -n "$version" || { echo "no version in Cargo.toml" >&2; exit 1; }

cargo build --release --locked -p sbrain --target "$TARGET"
bin="target/$TARGET/release/sbrain"

if [[ "$TARGET" == *-apple-darwin && -n "${SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$bin"
  codesign --verify --strict --verbose=2 "$bin"
  if [[ -n "${NOTARY_KEY_PATH:-}" ]]; then
    : "${NOTARY_KEY_ID:?set NOTARY_KEY_ID}" "${NOTARY_ISSUER:?set NOTARY_ISSUER}"
    work="$(mktemp -d)"
    ditto -c -k "$bin" "$work/sbrain.zip"
    xcrun notarytool submit "$work/sbrain.zip" \
      --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" \
      --wait --timeout 30m --output-format json | tee "$work/result.json"
    status="$(sed -n 's/.*"status":"\([^"]*\)".*/\1/p' "$work/result.json")"
    if [[ "$status" != "Accepted" ]]; then
      id="$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' "$work/result.json")"
      [[ -n "$id" ]] && xcrun notarytool log "$id" \
        --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" || true
      echo "notarization: ${status:-no answer}" >&2
      exit 1
    fi
    rm -rf "$work"
  fi
fi

case "$TARGET" in
  aarch64-apple-darwin) platform=macos-arm64 ;;
  x86_64-apple-darwin) platform=macos-x86_64 ;;
  aarch64-unknown-linux-gnu) platform=linux-arm64 ;;
  x86_64-unknown-linux-gnu) platform=linux-x86_64 ;;
  *) echo "no platform name for $TARGET" >&2; exit 1 ;;
esac
name="sbrain-$version-$platform"
rm -rf "dist/$name" "dist/$name.tar.gz"
mkdir -p "dist/$name"
cp "$bin" LICENSE NOTICE README.md "dist/$name/"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "dist/$name"
echo "dist/$name.tar.gz"
