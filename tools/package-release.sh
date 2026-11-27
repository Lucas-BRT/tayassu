#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 3 ]]; then
  printf 'usage: %s <binary> <target> <output-directory>\n' "$0" >&2
  exit 2
fi

binary="$1"
target="$2"
output_dir="$3"
case "$target" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
  *) printf 'unsupported target: %s\n' "$target" >&2; exit 2 ;;
esac

staging="$(mktemp -d)"
trap 'rm -rf -- "$staging"' EXIT
install -Dm755 "$binary" "$staging/tayassu"
mkdir -p "$output_dir"
archive="tayassu-$target.tar.gz"
tar -czf "$output_dir/$archive" -C "$staging" tayassu
(
  cd "$output_dir"
  sha256sum -- "$archive" > "$archive.sha256"
)
