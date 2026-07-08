use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
}

pub fn cached_corpus_root() -> PathBuf {
	let archive = fixtures_root().join("corpus.tar.gz");
	let archive_bytes = fs::read(&archive).expect("read corpus.tar.gz");
	let archive_hash = blake3::hash(&archive_bytes).to_hex().to_string();
	let root = repo_root()
		.join("target")
		.join("foch-merge-quality-fixtures")
		.join(format!("corpus-{}", &archive_hash[..16]));
	let marker = root.join(".archive-hash");
	if marker.is_file()
		&& fs::read_to_string(&marker).is_ok_and(|hash| hash == archive_hash)
		&& root.join("corpus.json").is_file()
	{
		return root;
	}

	let parent = root.parent().expect("cache parent");
	fs::create_dir_all(parent).expect("create fixture cache parent");
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let staging = parent.join(format!(".corpus-{}-{nanos}.tmp", std::process::id()));
	let _ = fs::remove_dir_all(&staging);
	foch_merge_quality::archive::unpack(&archive, &staging).expect("unpack corpus.tar.gz");
	fs::write(marker_path(&staging), archive_hash).expect("write archive hash marker");
	let _ = fs::remove_dir_all(&root);
	fs::rename(&staging, &root).expect("publish cached corpus fixture");
	root
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
