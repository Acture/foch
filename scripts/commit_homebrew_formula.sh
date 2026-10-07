#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 3 ]; then
	echo "usage: $0 <tap-dir> <formula-file> <version>" >&2
	exit 2
fi

formula_file="$(cd "$(dirname "$2")" && pwd -P)/$(basename "$2")"
version="$3"
name="$(basename "$formula_file")"

cd "$1"
if [ ! -d Formula ]; then
	echo "tap has no Formula directory" >&2
	exit 1
fi

# Write through the Homebrew entry, which may be a directory symlink. Git does
# not follow directory symlinks in pathspecs, so stage the physical path.
root="$(pwd -P)"
formula_dir="$(cd Formula && pwd -P)"
case "$formula_dir" in
	"$root"/?*) ;;
	*) echo "Formula resolves outside the tap: $formula_dir" >&2; exit 1 ;;
esac
path="${formula_dir#"$root"/}/$name"

cp "$formula_file" "Formula/$name"
git add -- "$path"
if git diff --cached --quiet -- "$path"; then
	echo "$path already up to date"
	exit 0
fi
git commit --quiet -m "${name%.rb} ${version}" -- "$path"
echo "Committed $path"
