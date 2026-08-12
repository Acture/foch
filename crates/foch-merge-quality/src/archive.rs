//! Read the frozen, committed integration fixture archive.
//!
//! Live Workshop inputs are never packed: product acceptance resolves them
//! directly from Steam's read-only installation.

use std::fs::File;
use std::io;
use std::path::Path;

use flate2::read::GzDecoder;

/// Unpack a gzip-compressed tar archive into `dest_dir`.
pub fn unpack(archive: &Path, dest_dir: &Path) -> io::Result<()> {
	std::fs::create_dir_all(dest_dir)?;
	let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
	tar.unpack(dest_dir)?;
	Ok(())
}
