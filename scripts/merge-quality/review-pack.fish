#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE review-pack
cargo test --locked --release -p foch-cli --test merge_quality_corpus review_pack_acceptance -- --ignored --exact --nocapture
