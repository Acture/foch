#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd $repo_root
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE review-pack
cargo test --release -p foch-cli --test merge_quality_corpus review_pack_acceptance -- --ignored --exact --nocapture
