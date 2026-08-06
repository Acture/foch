#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW structured-rollout
cargo test --locked -p foch-merge-quality --test maintenance structured_rollout_acceptance -- --ignored --exact --nocapture
