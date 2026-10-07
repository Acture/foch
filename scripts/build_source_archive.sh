#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
	echo "usage: $0 <version> <output-dir>" >&2
	exit 2
fi

if git submodule status | awk '$2 != "notes" && /^[-+U]/ { found = 1 } END { exit !found }'; then
	echo "public submodules must be checked out at their recorded commits" >&2
	exit 1
fi

root="foch-$1"
mkdir -p "$2"
output="$(cd "$2" && pwd -P)/${root}-source.tar.gz"

# Stage outside the checkout so the output never feeds back into the input.
stage="$(mktemp -d "${TMPDIR:-/tmp}/foch-source.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
# Keep macOS tar from adding AppleDouble files during local rehearsals.
export COPYFILE_DISABLE=1

# Archive tracked files and public submodules only: never the private notes
# submodule, nor untracked or ignored files from a local checkout.
git -c submodule.notes.active=false ls-files -z --recurse-submodules |
	rsync -a --from0 --files-from=- --exclude='/notes' ./ "$stage/$root/"
tar -C "$stage" -czf "$output" "$root"
echo "$output"
