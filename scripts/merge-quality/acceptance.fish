#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1

# The cohort must fit the product's normal per-layer 1 GiB cache contract.
# Acceptance is invalid if it succeeds only under an enlarged cache cap.
set -e FOCH_CACHE_MAX_BYTES

# The cache gate exercises the default plan preview and must not commit an output.
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE workshop-cache-residency-gate
cargo test --locked --release -p foch-cli --test merge_quality_corpus workshop_product_cache_residency_gate -- --ignored --exact --nocapture; or exit 1

# The fixed 14-case product gate passes --confirm explicitly and measures commits.
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE full-product-workshop
cargo test --locked --release -p foch-cli --test merge_quality_corpus workshop_product_corpus_acceptance -- --ignored --exact --nocapture; or exit 1
