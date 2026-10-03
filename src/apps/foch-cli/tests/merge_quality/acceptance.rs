//! Shell-independent orchestration for the explicit `cargo acceptance` entrypoint.

use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::time::Instant;

use super::{ACCEPTANCE_ENV, require_acceptance};

#[test]
#[ignore = "long real-Workshop validation; run cargo acceptance explicitly"]
fn workshop_product_acceptance() {
	require_acceptance("workshop-product-acceptance");
	let executable: PathBuf = std::env::current_exe().expect("locate acceptance test executable");
	for (test, authorization) in [
		(
			"workshop_product_cache_residency_gate",
			"workshop-cache-residency-gate",
		),
		(
			"workshop_product_corpus_acceptance",
			"full-product-workshop",
		),
	] {
		eprintln!("[acceptance] starting {test}");
		let started: Instant = Instant::now();
		let status: ExitStatus = Command::new(&executable)
			.args([test, "--ignored", "--exact", "--nocapture"])
			.env(ACCEPTANCE_ENV, authorization)
			// Acceptance must use the normal per-layer cache cap. Isolate each
			// stage's environment without mutating this multithreaded test process.
			.env_remove("FOCH_CACHE_MAX_BYTES")
			.status()
			.expect("start acceptance stage");
		assert!(status.success(), "acceptance stage {test} failed: {status}");
		eprintln!("[acceptance] completed {test} in {:?}", started.elapsed());
	}
}
