use std::fs;
use std::io;
use std::path::PathBuf;

use super::*;
use crate::game::eu4::Eu4;
use crate::game::eu4::base::snapshot::{
	BASE_DATA_DIR_ENV, BASE_DATA_ENV_LOCK, BaseDataSource, InstalledBaseSnapshotCommitGuard,
	build_base_snapshot, install_built_snapshot, installed_base_snapshot_identity,
	lock_and_validate_installed_base_snapshot_identity,
};
use crate::input::FileFilter;
use crate::input::config::Config;
use crate::input::request::InputRequest;
use crate::merge::analyze::{
	CancellationToken, MergeAnalysisOptions, NoopProgressObserver, analyze_merge,
	merge_execution_result,
};
use crate::model::{MERGE_REPORT_ARTIFACT_PATH, MergeReport};

fn analyze_merge_for_test(
	request: InputRequest,
	options: MergeAnalysisOptions,
) -> Result<AnalyzedMerge, MergeError> {
	analyze_merge(
		request,
		options,
		&NoopProgressObserver,
		&CancellationToken::new(),
	)
}

fn passthrough_options(out_dir: PathBuf) -> MergeAnalysisOptions {
	MergeAnalysisOptions {
		out_dir,
		include_game_base: false,
		include_base: false,
		gui_scroll_merge: false,
		force: false,
		ignore_replace_path: false,
		dep_overrides: Vec::new(),
		resolution_config_path: None,
		interactive_conflict_handler: None,
		interactive_resolution_config_path: None,
		playset_fingerprint: None,
		provenance: false,
		retained_paths: None,
	}
}

fn minimal_passthrough_fixture() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures/playsets/eu4_minimal_passthrough")
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn commit_requires_separate_replacement_authorization() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("out");
	let analyzed = analyze_merge_for_test(
		InputRequest::from_playset_path(
			minimal_passthrough_fixture().join("dlc_load.json"),
			Config::default(),
		),
		passthrough_options(out_dir.clone()),
	)
	.expect("analyze merge");
	fs::create_dir_all(&out_dir).expect("create output");
	fs::write(out_dir.join("user-file.txt"), b"preserve me\n").expect("seed output");

	let error = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect_err("replacement must require explicit authorization");

	assert!(matches!(
		error,
		MergeError::ReplacementAuthorizationRequired { .. }
	));
	assert_eq!(
		fs::read(out_dir.join("user-file.txt")).expect("read preserved output"),
		b"preserve me\n"
	);
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn commit_rejects_a_replacement_target_changed_after_confirmation() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("out");
	let analyzed = analyze_merge_for_test(
		InputRequest::from_playset_path(
			minimal_passthrough_fixture().join("dlc_load.json"),
			Config::default(),
		),
		passthrough_options(out_dir.clone()),
	)
	.expect("analyze merge");
	fs::create_dir_all(&out_dir).expect("create output");
	let user_file = out_dir.join("user-file.txt");
	fs::write(&user_file, b"confirmed bytes\n").expect("seed output");
	let replacement = analyzed
		.replacement_target()
		.expect("fingerprint output")
		.expect("non-empty output token");
	fs::write(&user_file, b"changed after confirmation\n").expect("mutate output");

	let error = analyzed
		.commit(CommitAuthorization::ReplaceExisting(replacement))
		.expect_err("changed output must invalidate replacement authorization");

	assert!(matches!(error, MergeError::ReplacementTargetChanged { .. }));
	assert_eq!(
		fs::read(&user_file).expect("read preserved changed output"),
		b"changed after confirmation\n"
	);
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn output_transaction_reports_the_prior_tree_it_observed() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");

	let missing = OutputTransaction::begin(&out_dir).expect("begin missing transaction");
	assert_eq!(missing.prior_dir(), None);
	drop(missing);

	fs::create_dir(&out_dir).expect("create prior output");
	fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write prior output");
	let existing = OutputTransaction::begin(&out_dir).expect("begin existing transaction");
	assert_eq!(existing.prior_dir(), Some(out_dir.as_path()));
}

#[test]
fn output_transaction_treats_an_existing_empty_directory_as_missing() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir(&out_dir).expect("create empty output");

	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	assert_eq!(transaction.prior_dir(), None);
	fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
		.expect("write staged output");
	transaction.commit().expect("commit transaction");

	assert_eq!(
		fs::read_to_string(out_dir.join("new.txt")).expect("read committed output"),
		"new output\n"
	);
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn output_transaction_replaces_the_complete_tree_without_overlay() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir_all(out_dir.join("common/governments")).expect("create old output");
	fs::write(out_dir.join("descriptor.mod"), "old descriptor\n").expect("write old descriptor");
	fs::write(
		out_dir.join("common/governments/stale.txt"),
		"stale government\n",
	)
	.expect("write stale module sibling");

	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	assert_eq!(transaction.staging_dir().parent(), out_dir.parent());
	fs::create_dir_all(transaction.staging_dir().join("common/governments"))
		.expect("create staged output");
	fs::write(
		transaction
			.staging_dir()
			.join("common/governments/current.txt"),
		"current government\n",
	)
	.expect("write staged module");
	transaction.commit().expect("commit transaction");

	assert_eq!(
		fs::read_to_string(out_dir.join("common/governments/current.txt"))
			.expect("read current module"),
		"current government\n"
	);
	assert!(!out_dir.join("common/governments/stale.txt").exists());
	assert!(!out_dir.join("descriptor.mod").exists());
}

#[test]
fn output_transaction_error_preserves_the_old_complete_tree() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir_all(out_dir.join("common/governments")).expect("create old output");
	fs::write(out_dir.join("descriptor.mod"), "old descriptor\n").expect("write old descriptor");
	fs::write(
		out_dir.join("common/governments/complete.txt"),
		"old complete module\n",
	)
	.expect("write old module");

	let result = (|| -> Result<(), MergeError> {
		let transaction = OutputTransaction::begin(&out_dir)?;
		fs::create_dir_all(transaction.staging_dir().join("common/governments"))?;
		fs::write(
			transaction
				.staging_dir()
				.join("common/governments/partial.txt"),
			"partial module\n",
		)?;
		Err(MergeError::Io(io::Error::other("injected failure")))
	})();

	assert!(result.is_err());
	assert_eq!(
		fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old descriptor"),
		"old descriptor\n"
	);
	assert_eq!(
		fs::read_to_string(out_dir.join("common/governments/complete.txt"))
			.expect("read old module"),
		"old complete module\n"
	);
	assert!(!out_dir.join("common/governments/partial.txt").exists());
}

#[test]
fn output_transaction_rejects_an_existing_regular_file() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::write(&out_dir, "do not replace\n").expect("write existing output file");

	let error = match OutputTransaction::begin(&out_dir) {
		Ok(_) => panic!("regular output file must be rejected"),
		Err(error) => error,
	};

	assert!(error.to_string().contains("must be a real directory"));
	assert_eq!(
		fs::read_to_string(&out_dir).expect("read preserved output file"),
		"do not replace\n"
	);
}

#[cfg(any(target_os = "windows", target_os = "redox"))]
#[test]
fn output_transaction_rejects_existing_directory_without_atomic_exchange() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir(&out_dir).expect("create existing output");
	fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write prior output");

	let error = match OutputTransaction::begin(&out_dir) {
		Ok(_) => panic!("existing output requires atomic directory exchange"),
		Err(error) => error,
	};

	assert!(
		error
			.to_string()
			.contains("atomic replacement of an existing output directory is unsupported")
	);
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn output_transaction_rejects_a_replaced_directory_before_commit() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir(&out_dir).expect("create initial output");
	fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write initial output");
	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
		.expect("write staged output");

	fs::remove_file(out_dir.join("prior.txt")).expect("remove initial output file");
	fs::remove_dir(&out_dir).expect("remove initial output");
	fs::create_dir(&out_dir).expect("create concurrent replacement");
	fs::write(out_dir.join("concurrent.txt"), "preserve me\n")
		.expect("write concurrent replacement");
	let error = transaction
		.commit()
		.expect_err("concurrent directory replacement must be rejected");

	assert!(
		error
			.to_string()
			.contains("changed while the replacement was staged")
	);
	assert_eq!(
		fs::read_to_string(out_dir.join("concurrent.txt")).expect("read concurrent replacement"),
		"preserve me\n"
	);
	assert!(!out_dir.join("new.txt").exists());
}

#[test]
fn output_transaction_drop_does_not_delete_a_replaced_staging_directory() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	let staging_dir = transaction.staging_dir().to_path_buf();

	fs::remove_dir(&staging_dir).expect("remove owned staging directory");
	fs::create_dir(&staging_dir).expect("create replacement staging directory");
	fs::write(staging_dir.join("sentinel.txt"), "preserve me\n")
		.expect("write replacement sentinel");
	drop(transaction);

	assert_eq!(
		fs::read_to_string(staging_dir.join("sentinel.txt"))
			.expect("replacement staging directory must survive"),
		"preserve me\n"
	);
}

#[test]
fn output_transactions_for_the_same_target_are_serialized() {
	use std::sync::{Arc, Barrier, mpsc};
	use std::thread;
	use std::time::Duration;

	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	let first = OutputTransaction::begin(&out_dir).expect("begin first transaction");
	let started = Arc::new(Barrier::new(2));
	let worker_barrier = Arc::clone(&started);
	let worker_out_dir = out_dir.clone();
	let (acquired_tx, acquired_rx) = mpsc::channel();
	let worker = thread::spawn(move || {
		worker_barrier.wait();
		let second = OutputTransaction::begin(&worker_out_dir).expect("begin second transaction");
		acquired_tx.send(()).expect("report acquired lock");
		drop(second);
	});

	started.wait();
	assert!(
		acquired_rx
			.recv_timeout(Duration::from_millis(100))
			.is_err(),
		"second transaction acquired the target lock before the first was dropped"
	);
	drop(first);
	acquired_rx
		.recv_timeout(Duration::from_secs(2))
		.expect("second transaction should acquire the released lock");
	worker.join().expect("join transaction worker");
}

#[cfg(unix)]
#[test]
fn output_transaction_rejects_an_existing_directory_symlink() {
	use std::os::unix::fs::symlink;

	let temp = tempfile::TempDir::new().expect("temp dir");
	let target = temp.path().join("actual-output");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir(&target).expect("create symlink target");
	fs::write(target.join("sentinel.txt"), "do not replace\n").expect("write sentinel");
	symlink(&target, &out_dir).expect("create output symlink");

	let error = match OutputTransaction::begin(&out_dir) {
		Ok(_) => panic!("output symlink must be rejected"),
		Err(error) => error,
	};

	assert!(error.to_string().contains("must be a real directory"));
	assert!(
		fs::symlink_metadata(&out_dir)
			.expect("read symlink")
			.file_type()
			.is_symlink()
	);
	assert_eq!(
		fs::read_to_string(target.join("sentinel.txt")).expect("read preserved target"),
		"do not replace\n"
	);
}

#[cfg(unix)]
#[test]
fn output_transaction_rejects_an_existing_unix_socket() {
	use std::os::unix::fs::FileTypeExt;
	use std::os::unix::net::UnixListener;

	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	let listener = UnixListener::bind(&out_dir).expect("bind output socket");

	let error = match OutputTransaction::begin(&out_dir) {
		Ok(_) => panic!("output socket must be rejected"),
		Err(error) => error,
	};

	assert!(error.to_string().contains("must be a real directory"));
	assert!(
		fs::symlink_metadata(&out_dir)
			.expect("read socket")
			.file_type()
			.is_socket()
	);
	drop(listener);
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn failed_commit_guard_does_not_commit_subset_output() {
	let temp = tempfile::TempDir::new().expect("temp dir");
	let out_dir = temp.path().join("merged-mod");
	fs::create_dir_all(&out_dir).expect("create old output");
	fs::write(out_dir.join("descriptor.mod"), "old descriptor\n").expect("write old descriptor");
	let base_snapshot = temp.path().join("base-snapshot.bin");
	fs::write(&base_snapshot, "base-v1").expect("write original base token");
	let expected_base = fs::read(&base_snapshot).expect("read original base token");

	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	fs::write(
		transaction.staging_dir().join("subset.txt"),
		"new subset output\n",
	)
	.expect("write staged subset");
	let execution = merge_execution_result(MergeReport::default());
	let result = finalize_merge_output(transaction, execution, |staging_dir| {
		assert!(staging_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
		fs::write(&base_snapshot, "base-v2")?;
		if fs::read(&base_snapshot)? != expected_base {
			return Err(MergeError::InputResolve {
				path: base_snapshot.clone(),
				message: "base snapshot changed before subset commit".to_string(),
			});
		}
		Ok(())
	});

	let error = result.expect_err("stale base must prevent subset commit");
	assert!(error.to_string().contains("base snapshot changed"));
	assert_eq!(
		fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old output"),
		"old descriptor\n"
	);
	assert!(!out_dir.join("subset.txt").exists());
}

#[test]
fn finalization_holds_the_commit_guard_through_commit() {
	use std::sync::Arc;
	use std::sync::atomic::{AtomicBool, Ordering};

	struct CommitGuardProbe {
		_guard: InstalledBaseSnapshotCommitGuard,
		alive: Arc<AtomicBool>,
	}

	impl Drop for CommitGuardProbe {
		fn drop(&mut self) {
			self.alive.store(false, Ordering::SeqCst);
		}
	}

	let _env_guard = BASE_DATA_ENV_LOCK.lock().expect("base data env lock");
	let temp = tempfile::TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path().join("base-data"));
	}

	let game = Eu4;
	let game_version = "1.37.5";
	let game_root = temp.path().join("eu4-game");
	fs::create_dir_all(game_root.join("common/scripted_triggers"))
		.expect("create base content root");
	fs::write(game_root.join("version.txt"), format!("{game_version}\n"))
		.expect("write game version");
	fs::write(
		game_root.join("common/scripted_triggers/base.txt"),
		"base_trigger = { always = yes }\n",
	)
	.expect("write base script");
	let filter = FileFilter::new(game, &[]).expect("build file filter");
	let built = build_base_snapshot(&game, &game_root, Some(game_version), &filter)
		.expect("build base snapshot");
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.expect("install base snapshot");
	let identity = installed_base_snapshot_identity(game.key(), game_version)
		.expect("read installed identity")
		.expect("installed identity exists");

	let out_dir = temp.path().join("merged-mod");
	let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
	fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
		.expect("write staged output");
	let execution = merge_execution_result(MergeReport::default());
	let guard_alive = Arc::new(AtomicBool::new(false));
	let validate_guard_alive = Arc::clone(&guard_alive);
	let commit_guard_alive = Arc::clone(&guard_alive);
	finalize_merge_output_with_commit(
		transaction,
		execution,
		|_| {
			let guard = lock_and_validate_installed_base_snapshot_identity(
				game.key(),
				game_version,
				&identity,
			)
			.map_err(|message| MergeError::InputResolve {
				path: game_root.clone(),
				message,
			})?;
			validate_guard_alive.store(true, Ordering::SeqCst);
			Ok(CommitGuardProbe {
				_guard: guard,
				alive: validate_guard_alive,
			})
		},
		|transaction| {
			assert!(
				commit_guard_alive.load(Ordering::SeqCst),
				"commit guard dropped before OutputTransaction::commit"
			);
			transaction.commit()
		},
	)
	.expect("finalize merge output");

	assert!(!guard_alive.load(Ordering::SeqCst));
	assert_eq!(
		fs::read_to_string(out_dir.join("new.txt")).expect("read committed output"),
		"new output\n"
	);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}
