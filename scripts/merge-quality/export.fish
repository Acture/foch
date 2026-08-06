#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW export
cargo test --locked -p foch-merge-quality --test maintenance export_dataset_metadata -- --ignored --exact --nocapture
