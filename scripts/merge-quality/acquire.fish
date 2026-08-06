#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW acquire
cargo test -p foch-merge-quality --features steam --test maintenance acquire_workshop_corpus -- --ignored --exact --nocapture
