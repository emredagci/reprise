#!/usr/bin/env bash
# Prints the Homebrew formula for VERSION from packaging/homebrew/sbrain.rb.in, with each archive's checksum
# taken from SHA256SUMS (as `sha256sum` writes it). Fails when an archive the formula names is missing.
#
#   ./.github/render-formula.sh 0.2.0 dist/SHA256SUMS > sbrain.rb
set -euo pipefail

version="${1:?usage: render-formula.sh VERSION SHA256SUMS}"
sums="${2:?usage: render-formula.sh VERSION SHA256SUMS}"
root="$(cd "$(dirname "$0")/.." && pwd)"
formula="$(cat "$root/packaging/homebrew/sbrain.rb.in")"
formula="${formula//@VERSION@/$version}"
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  sum="$(awk -v f="sbrain-$version-$target.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$sums")"
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "no checksum for sbrain-$version-$target.tar.gz in $sums" >&2; exit 1; }
  formula="${formula//@SHA256_$target@/$sum}"
done
if [[ "$formula" == *@* && "$formula" =~ @[A-Z0-9_a-z.-]+@ ]]; then
  echo "unfilled placeholder: ${BASH_REMATCH[0]}" >&2
  exit 1
fi
printf '%s\n' "$formula"
