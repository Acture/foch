use blake3::Hasher;
use std::{fs::File, io::{self, Read}};
use std::path::PathBuf;

pub fn strip_quotes(s: &str) -> Result<String, &str> {
	if s.starts_with('"') && s.ends_with('"') {
		Ok(s[1..s.len() - 1].to_string())
	} else {
		Err("String does not start and end with quotes")
	}
}

pub fn file_fingerprint(path: &PathBuf) -> Result<String, io::Error> {
	let mut file = File::open(path)?;
	let mut hasher = Hasher::new();
	let mut buf = [0u8; 8192];
	loop {
		let n = file.read(&mut buf)?;
		if n == 0 { break; }
		hasher.update(&buf[..n]);
	}
	Ok(hasher.finalize().to_hex().to_string())
}
