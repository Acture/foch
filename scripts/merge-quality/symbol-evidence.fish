#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW symbol-evidence
cargo test -p foch-merge-quality --test maintenance symbol_evidence -- --ignored --exact --nocapture
