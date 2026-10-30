#!/usr/bin/env bash
# Prints the Homebrew formula for VERSION from packaging/homebrew/reprise.rb.in, with each archive's checksum
# taken from SHA256SUMS (as `sha256sum` writes it). Fails when an archive the formula names is missing.
#
#   ./.github/render-formula.sh 0.2.0 dist/SHA256SUMS > reprise.rb
set -euo pipefail

version="${1:?usage: render-formula.sh VERSION SHA256SUMS}"
sums="${2:?usage: render-formula.sh VERSION SHA256SUMS}"
root="$(cd "$(dirname "$0")/.." && pwd)"
formula="$(cat "$root/packaging/homebrew/reprise.rb.in")"
formula="${formula//@VERSION@/$version}"
for platform in macos-arm64 macos-x86_64 linux-arm64 linux-x86_64; do
  sum="$(awk -v f="reprise-$version-$platform.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$sums")"
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "no checksum for reprise-$version-$platform.tar.gz in $sums" >&2; exit 1; }
  formula="${formula//@SHA256_$platform@/$sum}"
done
if [[ "$formula" == *@* && "$formula" =~ @[A-Z0-9_a-z.-]+@ ]]; then
  echo "unfilled placeholder: ${BASH_REMATCH[0]}" >&2
  exit 1
fi
printf '%s\n' "$formula"
