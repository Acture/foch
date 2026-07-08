use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_FORMAT: &str = "2";

static CACHED_CORPUS_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
}

pub fn cached_corpus_root() -> PathBuf {
	CACHED_CORPUS_ROOT
		.get_or_init(build_cached_corpus_root)
		.clone()
}

pub fn expected_verdicts() -> BTreeMap<String, BTreeMap<String, usize>> {
	let expected_text =
		fs::read_to_string(fixtures_root().join("expected.json")).expect("read expected.json");
	serde_json::from_str(&expected_text).expect("parse expected.json")
}

fn build_cached_corpus_root() -> PathBuf {
	let archive = fixtures_root().join("corpus.tar.gz");
	let archive_bytes = fs::read(&archive).expect("read corpus.tar.gz");
	let archive_hash = blake3::hash(&archive_bytes).to_hex().to_string();
	let root = repo_root()
		.join("target")
		.join("foch-merge-quality-fixtures")
		.join(format!("corpus-{}", &archive_hash[..16]));
	if cached_root_is_valid(&root, &archive_hash) {
		return root;
	}

	let parent = root.parent().expect("cache parent");
	fs::create_dir_all(parent).expect("create fixture cache parent");
	let lock_path = parent.join(format!(".corpus-{}.lock", &archive_hash[..16]));
	let _lock = acquire_cache_lock(&lock_path, &root, &archive_hash);
	if cached_root_is_valid(&root, &archive_hash) {
		return root;
	}

	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let staging = parent.join(format!(".corpus-{}-{nanos}.tmp", std::process::id()));
	let _ = fs::remove_dir_all(&staging);
	foch_merge_quality::archive::unpack(&archive, &staging).expect("unpack corpus.tar.gz");
	fs::write(marker_path(&staging), &archive_hash).expect("write archive hash marker");
	fs::write(format_path(&staging), CACHE_FORMAT).expect("write cache format marker");
	let _ = fs::remove_dir_all(&root);
	fs::rename(&staging, &root).expect("publish cached corpus fixture");
	root
}

fn cached_root_is_valid(root: &Path, archive_hash: &str) -> bool {
	marker_path(root).is_file()
		&& format_path(root).is_file()
		&& fs::read_to_string(marker_path(root)).is_ok_and(|hash| hash == archive_hash)
		&& fs::read_to_string(format_path(root)).is_ok_and(|version| version == CACHE_FORMAT)
		&& root.join("corpus.json").is_file()
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

fn acquire_cache_lock(lock_path: &Path, root: &Path, archive_hash: &str) -> CacheLock {
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
				if cached_root_is_valid(root, archive_hash) {
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
