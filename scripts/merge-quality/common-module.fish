#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW common-module
cargo test -p foch-merge-quality --test maintenance common_module_acceptance -- --ignored --exact --nocapture
