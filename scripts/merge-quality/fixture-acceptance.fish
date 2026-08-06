#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd $repo_root
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE product-fixture
cargo test --release -p foch-cli --test merge_quality_corpus product_fixture_acceptance -- --ignored --exact --nocapture
