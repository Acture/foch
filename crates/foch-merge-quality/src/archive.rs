//! Pack/unpack the committed corpus as a single gzip-compressed tar archive, so
//! the repo holds one compressed binary instead of hundreds of loose
//! third-party mod-file excerpts.
//!
//! Packing is **deterministic** — entries are sorted by path and all
//! mtime/uid/gid/mode metadata is zeroed — so the same slice tree always
//! produces byte-identical `corpus.tar.gz` (no git churn, verifiable diffs).

use std::fs::File;
use std::io;
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use walkdir::WalkDir;

/// Tar + gzip every file under `src_dir` into `out`, with deterministic bytes.
pub fn pack_dir(src_dir: &Path, out: &Path) -> io::Result<()> {
	if let Some(parent) = out.parent() {
		std::fs::create_dir_all(parent)?;
	}

	// Collect relative paths and sort for a stable archive order.
	let mut rels: Vec<String> = Vec::new();
	for entry in WalkDir::new(src_dir).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}
		if let Ok(rel) = entry.path().strip_prefix(src_dir) {
			rels.push(rel.to_string_lossy().replace('\\', "/"));
		}
	}
	rels.sort();

	let encoder = GzEncoder::new(File::create(out)?, Compression::default());
	let mut builder = tar::Builder::new(encoder);
	for rel in &rels {
		let data = std::fs::read(src_dir.join(rel))?;
		let mut header = tar::Header::new_gnu();
		header.set_size(data.len() as u64);
		header.set_mode(0o644);
		header.set_mtime(0);
		header.set_uid(0);
		header.set_gid(0);
		header.set_cksum();
		builder.append_data(&mut header, rel, data.as_slice())?;
	}
	builder.into_inner()?.finish()?;
	Ok(())
}

/// Unpack a gzip-compressed tar archive into `dest_dir`.
pub fn unpack(archive: &Path, dest_dir: &Path) -> io::Result<()> {
	std::fs::create_dir_all(dest_dir)?;
	let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
	tar.unpack(dest_dir)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn pack_then_unpack_roundtrips_and_is_deterministic() {
		let src = TempDir::new().unwrap();
		std::fs::create_dir_all(src.path().join("a/common")).unwrap();
		std::fs::write(src.path().join("a/common/x.txt"), b"hello\r\nworld\n").unwrap();
		std::fs::write(src.path().join("b.txt"), b"second").unwrap();

		let out = TempDir::new().unwrap();
		let arc1 = out.path().join("c1.tar.gz");
		let arc2 = out.path().join("c2.tar.gz");
		pack_dir(src.path(), &arc1).unwrap();
		pack_dir(src.path(), &arc2).unwrap();

		// Deterministic: same input → byte-identical archive.
		assert_eq!(
			std::fs::read(&arc1).unwrap(),
			std::fs::read(&arc2).unwrap(),
			"packing must be reproducible"
		);

		// Roundtrip: unpack restores exact bytes (incl. CRLF).
		let dest = TempDir::new().unwrap();
		unpack(&arc1, dest.path()).unwrap();
		assert_eq!(
			std::fs::read(dest.path().join("a/common/x.txt")).unwrap(),
			b"hello\r\nworld\n"
		);
		assert_eq!(std::fs::read(dest.path().join("b.txt")).unwrap(), b"second");
	}
}
