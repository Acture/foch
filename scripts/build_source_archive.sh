#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
	echo "usage: $0 <version> <output-dir>" >&2
	exit 2
fi

root="foch-$1"
mkdir -p "$2"
output="$(cd "$2" && pwd -P)/${root}-source.tar.gz"

# Stage outside the checkout; staging inside it makes rsync copy its own output.
stage="$(mktemp -d "${TMPDIR:-/tmp}/foch-source.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
# Keep macOS tar from adding AppleDouble files during local rehearsals.
export COPYFILE_DISABLE=1

rsync -a \
	--exclude='.git' \
	--exclude='.direnv' \
	--exclude='target' \
	--exclude='node_modules' \
	--exclude='/notes/' \
	--exclude='/dist/' \
	./ "$stage/$root/"
tar -C "$stage" -czf "$output" "$root"
echo "$output"
