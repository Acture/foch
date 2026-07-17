use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use foch_merge_quality::shadow::{ShadowCaptureRequest, ShadowRunRecord, capture_input_manifest};

#[test]
fn isolated_failed_arm_clears_stale_output() {
	let temp = tempfile::tempdir().unwrap();
	let data_root = temp.path().join("Europa Universalis IV");
	let game_root = temp.path().join("game");
	let mod_root = temp.path().join("mod-a");
	let mod_file = mod_root.join("events/a.txt");
	let base_file = game_root.join("events/a.txt");
	let executable = std::path::Path::new(env!("CARGO_BIN_EXE_foch-mq"));
	fs::create_dir_all(data_root.join("mod")).unwrap();
	fs::create_dir_all(mod_file.parent().unwrap()).unwrap();
	fs::create_dir_all(base_file.parent().unwrap()).unwrap();
	fs::write(&mod_file, "test.1 = { trigger = { always = yes } }\n").unwrap();
	fs::write(&base_file, "test.1 = { trigger = { always = no } }\n").unwrap();
	fs::write(
		mod_root.join("descriptor.mod"),
		"name=\"mod-a\"\nremote_file_id=\"1\"\n",
	)
	.unwrap();
	fs::write(
		data_root.join("mod/ugc_1.mod"),
		format!(
			"name=\"mod-a\"\npath=\"{}\"\nremote_file_id=\"1\"\n",
			mod_root.display()
		),
	)
	.unwrap();
	let playset = data_root.join("dlc_load.json");
	fs::write(
		&playset,
		r#"{"enabled_mods":["mod/ugc_1.mod"],"disabled_dlcs":[]}"#,
	)
	.unwrap();
	let retained_paths = BTreeSet::from(["events/a.txt".to_string()]);
	let retained_base_paths = BTreeSet::from(["events/a.txt".to_string()]);
	let manifest = capture_input_manifest(ShadowCaptureRequest {
		playset: &playset,
		game_root: &game_root,
		game_version: "shadow-test-no-base",
		retained_paths: &retained_paths,
		retained_base_paths: &retained_base_paths,
		base_snapshot_identity: "sha256:not-used",
		force: false,
		executable,
	})
	.unwrap();
	let manifest_path = temp.path().join("shadow-inputs.json");
	fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
	fs::write(&mod_file, "test.1 = { trigger = { always = no } }\n").unwrap();
	let output_dir = temp.path().join("structured");
	fs::create_dir_all(output_dir.join("events")).unwrap();
	fs::write(output_dir.join("events/stale.txt"), "stale").unwrap();

	let output = Command::new(executable)
		.arg("shadow-run-one")
		.arg("--input-manifest")
		.arg(&manifest_path)
		.arg("--out-dir")
		.arg(&output_dir)
		.arg("--kernel")
		.arg("structured")
		.output()
		.unwrap();

	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let record: ShadowRunRecord = serde_json::from_slice(&output.stdout).unwrap();
	assert_eq!(record.status, "error");
	assert!(!record.output_valid);
	assert!(!output_dir.exists());
	assert!(
		record
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("mod_contents"))
	);
}
