#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 <tag> <dist-data-dir>" >&2
  exit 1
fi

tag="$1"
dist_dir="$2"
manifest_path="${dist_dir}/foch-data-manifest.json"

if [[ ! -f "${manifest_path}" ]]; then
  echo "Missing manifest: ${manifest_path}" >&2
  exit 1
fi

mapfile -t asset_names < <(
  python3 - "${manifest_path}" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    manifest = json.load(handle)

for asset in manifest.get("assets", []):
    name = asset.get("asset_name")
    if name:
        print(name)
PY
)

assets=("${manifest_path}")
for asset_name in "${asset_names[@]}"; do
  asset_path="${dist_dir}/${asset_name}"
  if [[ ! -f "${asset_path}" ]]; then
    echo "Missing asset referenced by manifest: ${asset_path}" >&2
    exit 1
  fi
  assets+=("${asset_path}")
done

gh release upload "${tag}" "${assets[@]}" --clobber
