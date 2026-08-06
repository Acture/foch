use std::collections::BTreeSet;
use std::fs;

use foch_merge_quality::shadow::{
	ShadowCaptureRequest, capture_input_manifest, diff_output_dirs, output_content_hash,
	validate_shadow_manifest_identity,
};

#[test]
fn captured_manifest_is_self_validating_and_content_addressed() {
	let temp = tempfile::tempdir().unwrap();
	let data_root = temp.path().join("Europa Universalis IV");
	let game_root = temp.path().join("game");
	let mod_root = temp.path().join("mod-a");
	let mod_file = mod_root.join("events/a.txt");
	let base_file = game_root.join("events/a.txt");
	let executable = temp.path().join("foch-product");
	fs::create_dir_all(data_root.join("mod")).unwrap();
	fs::create_dir_all(mod_file.parent().unwrap()).unwrap();
	fs::create_dir_all(base_file.parent().unwrap()).unwrap();
	fs::write(&mod_file, "test.1 = { trigger = { always = yes } }\n").unwrap();
	fs::write(&base_file, "test.1 = { trigger = { always = no } }\n").unwrap();
	fs::write(&executable, "product-binary-v1").unwrap();
	fs::write(
		mod_root.join("descriptor.mod"),
		"name=\"mod-a\"\nremote_file_id=\"1\"\n",
	)
	.unwrap();
	fs::write(
		data_root.join("mod/ugc_1.mod"),
		format!(
			"name=\"mod-a\"\npath=\"{}\"\nremote_file_id=\"1\"\n",
			mod_root.to_string_lossy().replace('\\', "/")
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
	let manifest = capture_input_manifest(ShadowCaptureRequest {
		playset: &playset,
		game_root: &game_root,
		game_version: "shadow-test",
		retained_paths: &retained_paths,
		retained_base_paths: &retained_paths,
		base_snapshot_identity: "sha256:fixed-base",
		force: false,
		executable: &executable,
	})
	.unwrap();

	validate_shadow_manifest_identity(&manifest).unwrap();
	let mut tampered = manifest;
	tampered.inputs.game_version = "different".to_string();
	assert!(validate_shadow_manifest_identity(&tampered).is_err());
}

#[test]
fn output_evidence_hash_and_diff_ignore_internal_reports() {
	let legacy = tempfile::tempdir().unwrap();
	let product = tempfile::tempdir().unwrap();
	for root in [legacy.path(), product.path()] {
		fs::create_dir_all(root.join("events")).unwrap();
		fs::create_dir_all(root.join(".foch")).unwrap();
	}
	fs::write(legacy.path().join("events/a.txt"), "legacy").unwrap();
	fs::write(product.path().join("events/a.txt"), "product").unwrap();
	fs::write(legacy.path().join(".foch/report.json"), "legacy-report").unwrap();
	fs::write(product.path().join(".foch/report.json"), "product-report").unwrap();

	let legacy_hash = output_content_hash(legacy.path()).unwrap().unwrap();
	fs::write(legacy.path().join(".foch/report.json"), "changed-report").unwrap();
	assert_eq!(
		output_content_hash(legacy.path()).unwrap().unwrap(),
		legacy_hash
	);
	let deltas = diff_output_dirs(legacy.path(), product.path()).unwrap();
	assert_eq!(deltas.len(), 1);
	assert_eq!(deltas[0].relative_path, "events/a.txt");
}
