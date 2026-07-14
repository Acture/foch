use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_FORMAT: &str = "3.0.0";

#[allow(
	dead_code,
	reason = "used only by the mod-only integration-test binary"
)]
static CACHED_CORPUS_ROOT: OnceLock<PathBuf> = OnceLock::new();
#[allow(
	dead_code,
	reason = "used only by the base-aware integration-test binary"
)]
static CACHED_BASE_AWARE_CORPUS_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
}

#[allow(
	dead_code,
	reason = "used only by the mod-only integration-test binary"
)]
pub fn cached_corpus_root() -> PathBuf {
	CACHED_CORPUS_ROOT
		.get_or_init(build_cached_corpus_root)
		.clone()
}

#[allow(
	dead_code,
	reason = "used only by the base-aware integration-test binary"
)]
pub fn cached_base_aware_corpus_root() -> PathBuf {
	CACHED_BASE_AWARE_CORPUS_ROOT
		.get_or_init(build_cached_base_aware_corpus_root)
		.clone()
}

pub fn expected_verdicts() -> BTreeMap<String, BTreeMap<String, usize>> {
	let expected_text =
		fs::read_to_string(fixtures_root().join("expected.json")).expect("read expected.json");
	serde_json::from_str(&expected_text).expect("parse expected.json")
}

#[allow(
	dead_code,
	reason = "used only by the mod-only integration-test binary"
)]
fn build_cached_corpus_root() -> PathBuf {
	let archives = [fixtures_root().join("corpus.tar.gz")];
	build_cached_root("corpus", &archives, false)
}

#[allow(
	dead_code,
	reason = "used only by the base-aware integration-test binary"
)]
fn build_cached_base_aware_corpus_root() -> PathBuf {
	let archives = [
		fixtures_root().join("corpus.tar.gz"),
		fixtures_root().join("basegame-text.tar.gz"),
	];
	build_cached_root("base-aware-corpus", &archives, true)
}

fn build_cached_root(namespace: &str, archives: &[PathBuf], base_aware: bool) -> PathBuf {
	let archive_hash = fixture_archive_hash(archives);
	let root = repo_root()
		.join("target")
		.join("foch-merge-quality-fixtures")
		.join(format!("{namespace}-{}", &archive_hash[..16]));
	if cached_root_is_valid(&root, &archive_hash, base_aware) {
		return root;
	}

	let parent = root.parent().expect("cache parent");
	fs::create_dir_all(parent).expect("create fixture cache parent");
	let lock_path = parent.join(format!(".{namespace}-{}.lock", &archive_hash[..16]));
	let _lock = acquire_cache_lock(&lock_path, &root, &archive_hash, base_aware);
	if cached_root_is_valid(&root, &archive_hash, base_aware) {
		return root;
	}

	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let staging = parent.join(format!(".{namespace}-{}-{nanos}.tmp", std::process::id()));
	let _ = fs::remove_dir_all(&staging);
	for archive in archives {
		foch_merge_quality::archive::unpack(archive, &staging)
			.unwrap_or_else(|err| panic!("unpack {}: {err}", archive.display()));
	}
	fs::write(marker_path(&staging), &archive_hash).expect("write archive hash marker");
	fs::write(format_path(&staging), CACHE_FORMAT).expect("write cache format marker");
	let _ = fs::remove_dir_all(&root);
	fs::rename(&staging, &root).expect("publish cached corpus fixture");
	prune_stale_fixture_caches(parent, &root, namespace);
	root
}

fn fixture_archive_hash(archives: &[PathBuf]) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-merge-quality-fixture-archives-v1");
	for archive in archives {
		let name = archive
			.file_name()
			.and_then(|name| name.to_str())
			.expect("fixture archive has a UTF-8 file name");
		let bytes = fs::read(archive)
			.unwrap_or_else(|err| panic!("read fixture archive {}: {err}", archive.display()));
		hasher.update(&(name.len() as u64).to_le_bytes());
		hasher.update(name.as_bytes());
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(&bytes);
	}
	hasher.finalize().to_hex().to_string()
}

fn cached_root_is_valid(root: &Path, archive_hash: &str, base_aware: bool) -> bool {
	marker_path(root).is_file()
		&& format_path(root).is_file()
		&& fs::read_to_string(marker_path(root)).is_ok_and(|hash| hash == archive_hash)
		&& fs::read_to_string(format_path(root)).is_ok_and(|version| version == CACHE_FORMAT)
		&& root.join("corpus.json").is_file()
		&& root.join("workshop").is_dir()
		&& (!base_aware
			|| (root.join("basegame/version.txt").is_file()
				&& root.join("basegame-manifest.json").is_file()))
}

fn prune_stale_fixture_caches(parent: &Path, current: &Path, namespace: &str) {
	let Ok(entries) = fs::read_dir(parent) else {
		return;
	};
	let prefix = format!("{namespace}-");
	for entry in entries.flatten() {
		let path = entry.path();
		if path == current || !path.is_dir() {
			continue;
		}
		let name = entry.file_name();
		if name.to_string_lossy().starts_with(&prefix) {
			let _ = fs::remove_dir_all(path);
		}
	}
}

struct CacheLock {
	owned: bool,
	path: PathBuf,
}

impl Drop for CacheLock {
	fn drop(&mut self) {
		if self.owned {
			let _ = fs::remove_dir(&self.path);
		}
	}
}

fn acquire_cache_lock(
	lock_path: &Path,
	root: &Path,
	archive_hash: &str,
	base_aware: bool,
) -> CacheLock {
	let started_at = Instant::now();
	loop {
		match fs::create_dir(lock_path) {
			Ok(()) => {
				return CacheLock {
					owned: true,
					path: lock_path.to_path_buf(),
				};
			}
			Err(err) if err.kind() == ErrorKind::AlreadyExists => {
				if cached_root_is_valid(root, archive_hash, base_aware) {
					return CacheLock {
						owned: false,
						path: lock_path.to_path_buf(),
					};
				}
				if started_at.elapsed() > Duration::from_secs(120) {
					let _ = fs::remove_dir_all(lock_path);
				}
				thread::sleep(Duration::from_millis(50));
			}
			Err(err) => panic!("acquire fixture cache lock: {err}"),
		}
	}
}

fn repo_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.and_then(Path::parent)
		.expect("repo root")
		.to_path_buf()
}

fn marker_path(root: &Path) -> PathBuf {
	root.join(".archive-hash")
}

fn format_path(root: &Path) -> PathBuf {
	root.join(".cache-format")
}

#[cfg(test)]
mod tests {
	use super::{CACHE_FORMAT, cached_root_is_valid, format_path, marker_path};
	use std::fs;

	#[test]
	fn mod_corpus_cache_does_not_require_local_basegame_payload() {
		let temp = tempfile::tempdir().expect("temp dir");
		fs::write(marker_path(temp.path()), "archive-hash").expect("write hash");
		fs::write(format_path(temp.path()), CACHE_FORMAT).expect("write format");
		fs::write(temp.path().join("corpus.json"), "{}").expect("write corpus");
		fs::create_dir(temp.path().join("workshop")).expect("create workshop");

		assert!(cached_root_is_valid(temp.path(), "archive-hash", false));
		assert!(!cached_root_is_valid(temp.path(), "archive-hash", true));
	}

	#[test]
	fn cache_format_mismatch_invalidates_old_fixture_root() {
		let temp = tempfile::tempdir().expect("temp dir");
		fs::write(marker_path(temp.path()), "archive-hash").expect("write hash");
		fs::write(format_path(temp.path()), "2.0.0").expect("write old format");
		fs::write(temp.path().join("corpus.json"), "{}").expect("write corpus");
		fs::create_dir(temp.path().join("workshop")).expect("create workshop");

		assert!(!cached_root_is_valid(temp.path(), "archive-hash", false));
	}
}
