#!/usr/bin/env fish

set -l repo_root (path resolve (dirname (status filename))/../..)
cd "$repo_root"; or exit 1
command -q /usr/bin/sandbox-exec; or begin
	echo "sandbox-exec is required to prove the product acceptance cannot read the legacy CAS" >&2
	exit 1
end

set -l legacy_objects "$repo_root/crates/foch-merge-quality/dataset/objects"
set -l legacy_work "$repo_root/crates/foch-merge-quality/dataset/.work"
set -l escaped_objects (string replace -a '\\' '\\\\' -- "$legacy_objects")
set escaped_objects (string replace -a '"' '\\"' -- "$escaped_objects")
set -l escaped_work (string replace -a '\\' '\\\\' -- "$legacy_work")
set escaped_work (string replace -a '"' '\\"' -- "$escaped_work")
set -l sandbox_profile "(version 1)
(allow default)
(deny file-read* file-write*
	(literal \"$escaped_objects\")
	(subpath \"$escaped_objects\")
	(literal \"$escaped_work\")
	(subpath \"$escaped_work\"))"

set -lx FOCH_LEGACY_CAS_GUARD seatbelt-v1
# The cohort must fit the product's normal per-layer 1 GiB cache contract.
# Acceptance is invalid if it succeeds only under an enlarged cache cap.
set -e FOCH_CACHE_MAX_BYTES

# The cache gate exercises the default plan preview and must not export an output.
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE workshop-cache-residency-gate
/usr/bin/sandbox-exec -p "$sandbox_profile" cargo test --locked --release -p foch-cli --test merge_quality_corpus workshop_product_cache_residency_gate -- --ignored --exact --nocapture; or exit 1

# The fixed 14-case product gate passes --confirm explicitly and measures exports.
set -lx FOCH_MERGE_QUALITY_ACCEPTANCE full-product-workshop
/usr/bin/sandbox-exec -p "$sandbox_profile" cargo test --locked --release -p foch-cli --test merge_quality_corpus workshop_product_corpus_acceptance -- --ignored --exact --nocapture; or exit 1
