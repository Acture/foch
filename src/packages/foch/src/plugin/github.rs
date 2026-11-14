//! Pinned GitHub releases for the Steam DLL plugins Foch installs.
//!
//! `foch plugin install` downloads one of these assets with `gh release download`.
//! The management process never scans the game `plugins` directory. Proxy loaders
//! are dropped by [`super::store::adapt`]; each release also names files that must
//! not be stored, such as EU4dll's x86 `Plugin.dll` and its autoupdaters.

use super::store::{self, ArchiveEntry, ImportError, ValidatedPackage};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

/// One upstream release Foch knows how to install.
///
/// The fields come from [`super::registry`]: one row per plugin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
	pub repo: String,
	pub tag: String,
	pub asset: String,
	/// Built-in adapter id the asset is bound to.
	pub adapter: String,
	/// Package-relative paths dropped before the adapter is bound.
	pub omit: Vec<String>,
}

const MAX_ENTRIES: usize = 256;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// The registered releases, in registry order.
pub fn releases() -> &'static [Release] {
	static RELEASES: OnceLock<Vec<Release>> = OnceLock::new();
	RELEASES
		.get_or_init(|| {
			super::registry::plugins()
				.iter()
				.map(|plugin| Release {
					repo: plugin.repo.clone(),
					tag: plugin.tag.clone(),
					asset: plugin.asset.clone(),
					adapter: plugin.id.clone(),
					omit: plugin.omit.clone(),
				})
				.collect()
		})
		.as_slice()
}

/// Why a spec or a release asset cannot be installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GithubError {
	UnknownSpec(String),
	TagMismatch { spec: String, pinned: String },
	Zip(String),
	Import(Vec<ImportError>),
	MissingAdapter(String),
}

impl std::fmt::Display for GithubError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::UnknownSpec(spec) => write!(
				f,
				"unknown plugin install spec {spec}. Pass owner/repo, a github.com URL, a built-in plugin id, or --all"
			),
			Self::TagMismatch { spec, pinned } => {
				write!(f, "{spec} is not the pinned release {pinned}")
			}
			Self::Zip(message) => write!(f, "could not read release zip: {message}"),
			Self::Import(errors) => {
				let mut message = String::from("package failed validation:");
				for error in errors {
					message.push_str(&format!("\n  - {error}"));
				}
				write!(f, "{message}")
			}
			Self::MissingAdapter(id) => write!(f, "built-in adapter {id} is not registered"),
		}
	}
}

impl std::error::Error for GithubError {}

/// Resolve `owner/repo`, a github.com URL, or a built-in plugin id.
pub fn resolve(spec: &str) -> Result<&'static Release, GithubError> {
	let spec = spec.trim();
	if let Some(release) = releases()
		.iter()
		.find(|release| release.adapter.eq_ignore_ascii_case(spec))
	{
		return Ok(release);
	}
	let Some((repo, tag)) = repository(spec) else {
		return Err(GithubError::UnknownSpec(spec.to_string()));
	};
	let Some(release) = releases()
		.iter()
		.find(|release| release.repo.eq_ignore_ascii_case(&repo))
	else {
		return Err(GithubError::UnknownSpec(spec.to_string()));
	};
	if let Some(tag) = tag
		&& tag != release.tag
	{
		return Err(GithubError::TagMismatch {
			spec: format!("{repo}@{tag}"),
			pinned: format!("{}@{}", release.repo, release.tag),
		});
	}
	Ok(release)
}

/// A release resolved dynamically from a repository, outside the
/// built-in registry: `foch plugin install Owner/Repo[@vX.Y.Z|@latest]`.
#[derive(Clone, Debug)]
pub struct DynamicRelease {
	pub repo: String,
	pub tag: String,
	pub asset: String,
}

/// Resolve any repository's release with the GitHub CLI. `reference` is
/// a tag, `latest` for the newest release, or `None` for latest.
pub fn resolve_dynamic(repo: &str, reference: Option<&str>) -> Result<DynamicRelease, GithubError> {
	let repo = repo.trim().trim_end_matches(".git");
	if repo.is_empty()
		|| !repo
			.split('/')
			.all(|segment| !segment.is_empty() && !segment.starts_with('.'))
	{
		return Err(GithubError::UnknownSpec(repo.to_string()));
	}
	let tag = match reference {
		None | Some("latest") => None,
		Some(tag)
			if tag.starts_with('v') || tag.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
		{
			Some(tag.to_string())
		}
		Some(other) => {
			return Err(GithubError::UnknownSpec(format!(
				"{repo}@{other}: install references are release tags or latest"
			)));
		}
	};
	let mut args = vec!["release".to_string(), "view".to_string()];
	if let Some(tag) = &tag {
		args.push(tag.clone());
	}
	args.push("--repo".to_string());
	args.push(repo.to_string());
	args.push("--json".to_string());
	args.push("tagName,assets".to_string());
	let output = std::process::Command::new("gh")
		.args(&args)
		.output()
		.map_err(|_| GithubError::UnknownSpec("gh was not found on PATH".to_string()))?;
	if !output.status.success() {
		return Err(GithubError::UnknownSpec(format!(
			"gh release view failed for {repo}: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}
	let view: serde_json::Value = serde_json::from_slice(&output.stdout)
		.map_err(|error| GithubError::UnknownSpec(format!("gh returned invalid JSON: {error}")))?;
	let resolved_tag = view
		.get("tagName")
		.and_then(|value| value.as_str())
		.unwrap_or_default()
		.to_string();
	let short = repo.rsplit('/').next().unwrap_or_default();
	let assets: Vec<String> = view
		.get("assets")
		.and_then(|value| value.as_array())
		.map(|entries| {
			entries
				.iter()
				.filter_map(|entry| entry.get("name").and_then(|name| name.as_str()))
				.map(str::to_string)
				.collect()
		})
		.unwrap_or_default();
	// Prefer a zip whose name carries the repository's, then any zip.
	let asset = assets
		.iter()
		.find(|name| name.ends_with(".zip") && name.to_lowercase().contains(&short.to_lowercase()))
		.or_else(|| assets.iter().find(|name| name.ends_with(".zip")))
		.ok_or_else(|| {
			GithubError::UnknownSpec(format!(
				"{repo}@{resolved_tag} ships no zip asset (assets: {})",
				assets.join(", ")
			))
		})?
		.clone();
	Ok(DynamicRelease {
		repo: repo.to_string(),
		tag: resolved_tag,
		asset,
	})
}

/// Split an install spec into `(repo, reference)`; `owner/repo` URLs and
/// `@latest` are accepted, and a missing reference means latest.
pub fn split_install_spec(spec: &str) -> Result<(String, Option<String>), GithubError> {
	let spec = spec.trim();
	let (repo, reference) = match spec.rsplit_once('@') {
		Some((repo, reference)) => (repo, Some(reference.to_string())),
		None => (spec, None),
	};
	let mut repo = repo.trim().trim_end_matches(".git").to_string();
	if let Some((_, after)) = repo.split_once("://") {
		repo = after.to_string();
	} else if let Some((_, after)) = repo.split_once(':') {
		repo = after.to_string();
	}
	let segments: Vec<&str> = repo.split('/').filter(|s| !s.is_empty()).collect();
	if segments.first().is_some_and(|first| first.contains('.')) {
		// Strip the host of a full URL.
		repo = segments[1.min(segments.len() - 1)..].join("/");
	}
	if repo.split('/').count() != 2 {
		return Err(GithubError::UnknownSpec(spec.to_string()));
	}
	Ok((repo, reference))
}

/// Arguments for `gh release download`, excluding the `gh` program name.
pub fn download_args(repo: &str, tag: &str, asset: &str, dest: &Path) -> Vec<OsString> {
	vec![
		"release".into(),
		"download".into(),
		tag.into(),
		"--repo".into(),
		repo.into(),
		"--pattern".into(),
		asset.into(),
		"--dir".into(),
		dest.as_os_str().to_os_string(),
		"--clobber".into(),
	]
}

/// Extract a release zip, drop its omitted paths, and bind the built-in adapter.
/// A registry release: the zip carries a foreign DLL, the manifest is
/// the built-in adapter's and `omit` drops adapter-bound files.
pub fn package_from_zip(bytes: &[u8], release: &Release) -> Result<ValidatedPackage, GithubError> {
	let mut entries = extract_zip(bytes)?;
	let omit: BTreeSet<String> = release
		.omit
		.iter()
		.map(|path| {
			store::safe_relative(path)
				.unwrap_or_else(|_| panic!("omit path is unsafe: {path}"))
				.to_ascii_lowercase()
		})
		.collect();
	entries.retain(|entry| !omit.contains(&entry.path.to_ascii_lowercase()));
	let manifest = super::builtin::adapter(&release.adapter)
		.ok_or_else(|| GithubError::MissingAdapter(release.adapter.to_string()))?;
	store::adapt(manifest, entries).map_err(GithubError::Import)
}

/// A repository's own release: the zip already carries its
/// `foch-plugin.toml`, so the package validates as-is.
pub fn package_from_own_zip(bytes: &[u8]) -> Result<ValidatedPackage, GithubError> {
	let entries = extract_zip(bytes)?;
	store::validate(entries).map_err(GithubError::Import)
}

fn repository(spec: &str) -> Option<(String, Option<String>)> {
	let spec = spec.trim().trim_end_matches('/');
	if spec.is_empty() {
		return None;
	}
	let lower = spec.to_ascii_lowercase();
	if lower.starts_with("https://") || lower.starts_with("http://") {
		let rest = spec.split_once("://")?.1;
		let (host, path) = rest.split_once('/')?;
		let host = host.split('@').next_back().unwrap_or(host);
		let host = host.split(':').next().unwrap_or(host);
		if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com")
		{
			return None;
		}
		let path = path.split(['?', '#']).next().unwrap_or(path);
		return github_path(path);
	}
	if lower.starts_with("git@github.com:") {
		let path = spec.split_once(':')?.1;
		let path = path.trim_end_matches(".git").trim_end_matches('/');
		let mut parts = path.split('/');
		let owner = parts.next()?;
		let repo = parts.next()?;
		if parts.next().is_some() || owner.is_empty() || repo.is_empty() {
			return None;
		}
		return Some((format!("{owner}/{repo}"), None));
	}
	if spec.contains("://") || spec.contains('\\') {
		return None;
	}
	github_path(spec)
}

/// `owner/repo`, optionally followed by `releases/tag/<tag>`.
fn github_path(path: &str) -> Option<(String, Option<String>)> {
	let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
	if parts.len() < 2 {
		return None;
	}
	let owner = parts[0];
	let repo = parts[1].trim_end_matches(".git");
	if owner.is_empty() || repo.is_empty() || owner.contains(' ') || repo.contains(' ') {
		return None;
	}
	let tag = match parts.len() {
		2 => None,
		5 if parts[2].eq_ignore_ascii_case("releases") && parts[3].eq_ignore_ascii_case("tag") => {
			Some(parts[4].to_string())
		}
		_ => return None,
	};
	Some((format!("{owner}/{repo}"), tag))
}

fn extract_zip(bytes: &[u8]) -> Result<Vec<ArchiveEntry>, GithubError> {
	let eocd = find_eocd(bytes)?;
	if eocd >= 20 && read_u32(bytes, eocd - 20)? == 0x0706_4b50 {
		return Err(zip("zip64 archives are not supported"));
	}
	let disk = read_u16(bytes, eocd + 4)?;
	let cd_disk = read_u16(bytes, eocd + 6)?;
	let records_here = read_u16(bytes, eocd + 8)?;
	let records = read_u16(bytes, eocd + 10)?;
	let cd_size = read_u32(bytes, eocd + 12)?;
	let cd_offset = read_u32(bytes, eocd + 16)?;
	if disk != 0 || cd_disk != 0 || records_here != records {
		return Err(zip("spanned archives are not supported"));
	}
	if cd_offset == u32::MAX || cd_size == u32::MAX || records == u16::MAX {
		return Err(zip("zip64 archives are not supported"));
	}
	if records as usize > MAX_ENTRIES {
		return Err(zip("release zip has too many entries"));
	}
	let cd_start = cd_offset as usize;
	let cd_end = cd_start
		.checked_add(cd_size as usize)
		.ok_or_else(|| zip("central directory is truncated"))?;
	if cd_end > eocd {
		return Err(zip("central directory is truncated"));
	}
	let members = read_central_directory(bytes, cd_start, cd_end, records as usize)?;
	let mut entries = Vec::with_capacity(members.len());
	let mut seen = BTreeSet::new();
	let mut total = 0usize;
	for member in members {
		let path = entry_path(&member.name)?;
		let lower = path.to_ascii_lowercase();
		if !seen.insert(lower) {
			return Err(zip(format!("duplicate path in release zip: {path}")));
		}
		if total.saturating_add(member.uncompressed as usize) > MAX_TOTAL_BYTES {
			return Err(zip("release zip expands past the size limit"));
		}
		let data = read_member(bytes, &member, cd_start)?;
		total += data.len();
		entries.push(ArchiveEntry { path, data });
	}
	Ok(entries)
}

struct Member {
	name: String,
	flags: u16,
	method: u16,
	crc: u32,
	compressed: u32,
	uncompressed: u32,
	local_offset: u32,
}

fn read_central_directory(
	bytes: &[u8],
	mut cursor: usize,
	end: usize,
	expected: usize,
) -> Result<Vec<Member>, GithubError> {
	let mut members = Vec::new();
	let mut records = 0usize;
	while cursor < end {
		if read_u32(bytes, cursor)? != 0x0201_4b50 {
			return Err(zip("central directory entry is truncated or corrupt"));
		}
		if cursor + 46 > end {
			return Err(zip("central directory entry is truncated or corrupt"));
		}
		let flags = read_u16(bytes, cursor + 8)?;
		let method = read_u16(bytes, cursor + 10)?;
		let crc = read_u32(bytes, cursor + 16)?;
		let compressed = read_u32(bytes, cursor + 20)?;
		let uncompressed = read_u32(bytes, cursor + 24)?;
		let name_len = read_u16(bytes, cursor + 28)? as usize;
		let extra_len = read_u16(bytes, cursor + 30)? as usize;
		let comment_len = read_u16(bytes, cursor + 32)? as usize;
		let local_offset = read_u32(bytes, cursor + 42)?;
		if compressed == u32::MAX || uncompressed == u32::MAX || local_offset == u32::MAX {
			return Err(zip("zip64 archives are not supported"));
		}
		let name_at = cursor + 46;
		let name_end = name_at
			.checked_add(name_len)
			.ok_or_else(|| zip("central directory entry is truncated or corrupt"))?;
		let next = name_end
			.checked_add(extra_len)
			.and_then(|offset| offset.checked_add(comment_len))
			.ok_or_else(|| zip("central directory entry is truncated or corrupt"))?;
		if next > end {
			return Err(zip("central directory entry is truncated or corrupt"));
		}
		let name_bytes = &bytes[name_at..name_end];
		if flags & 1 != 0 {
			return Err(zip("encrypted release entries are not supported"));
		}
		if method != 0 && method != 8 {
			return Err(zip(format!("unsupported compression method {method}")));
		}
		let name = decode_name(name_bytes, flags)?;
		let directory = name.ends_with('/') || name.ends_with('\\');
		cursor = next;
		records += 1;
		if directory {
			if uncompressed != 0 {
				return Err(zip(format!("directory entry has data: {name}")));
			}
			continue;
		}
		if uncompressed as usize > MAX_FILE_BYTES {
			return Err(zip(format!("release entry is too large: {name}")));
		}
		members.push(Member {
			name,
			flags,
			method,
			crc,
			compressed,
			uncompressed,
			local_offset,
		});
	}
	if cursor != end || records != expected {
		return Err(zip("central directory size does not match its entries"));
	}
	Ok(members)
}

fn read_member(bytes: &[u8], member: &Member, cd_start: usize) -> Result<Vec<u8>, GithubError> {
	let offset = member.local_offset as usize;
	if offset >= cd_start || read_u32(bytes, offset)? != 0x0403_4b50 {
		return Err(zip(format!("local header is missing for {}", member.name)));
	}
	let flags = read_u16(bytes, offset + 6)?;
	let method = read_u16(bytes, offset + 8)?;
	let local_compressed = read_u32(bytes, offset + 18)?;
	let local_uncompressed = read_u32(bytes, offset + 22)?;
	let name_len = read_u16(bytes, offset + 26)? as usize;
	let extra_len = read_u16(bytes, offset + 28)? as usize;
	let name_at = offset + 30;
	let name_end = name_at
		.checked_add(name_len)
		.ok_or_else(|| zip(format!("local header is truncated for {}", member.name)))?;
	let local_name = bytes
		.get(name_at..name_end)
		.ok_or_else(|| zip(format!("local header is truncated for {}", member.name)))?;
	if decode_name(local_name, flags)? != member.name || method != member.method {
		return Err(zip(format!(
			"local header does not match the central directory for {}",
			member.name
		)));
	}
	if flags & 1 != 0 {
		return Err(zip("encrypted release entries are not supported"));
	}
	let descriptor = member.flags & 8 != 0 || flags & 8 != 0;
	if !descriptor
		&& (local_compressed != member.compressed || local_uncompressed != member.uncompressed)
	{
		return Err(zip(format!(
			"local sizes do not match the central directory for {}",
			member.name
		)));
	}
	let data_at = name_end
		.checked_add(extra_len)
		.ok_or_else(|| zip(format!("local header is truncated for {}", member.name)))?;
	let data_end = data_at
		.checked_add(member.compressed as usize)
		.ok_or_else(|| zip(format!("compressed data is truncated for {}", member.name)))?;
	if data_end > cd_start {
		return Err(zip(format!(
			"compressed data runs into the central directory for {}",
			member.name
		)));
	}
	let data = bytes
		.get(data_at..data_end)
		.ok_or_else(|| zip(format!("compressed data is truncated for {}", member.name)))?;
	let inflated = inflate(member, data)?;
	if crc32(&inflated) != member.crc {
		return Err(zip(format!("crc32 mismatch for {}", member.name)));
	}
	Ok(inflated)
}

fn inflate(member: &Member, data: &[u8]) -> Result<Vec<u8>, GithubError> {
	let expected = member.uncompressed as usize;
	match member.method {
		0 => {
			if data.len() != expected {
				return Err(zip(format!("stored size mismatch for {}", member.name)));
			}
			Ok(data.to_vec())
		}
		8 => {
			let mut decoder = flate2::read::DeflateDecoder::new(data);
			let mut out = Vec::new();
			let mut buf = [0u8; 8192];
			loop {
				let read = decoder
					.read(&mut buf)
					.map_err(|error| zip(format!("deflate failed for {}: {error}", member.name)))?;
				if read == 0 {
					break;
				}
				if out.len() + read > expected {
					return Err(zip(format!(
						"deflate output exceeds the declared size for {}",
						member.name
					)));
				}
				out.extend_from_slice(&buf[..read]);
			}
			if out.len() != expected || decoder.total_in() != member.compressed as u64 {
				return Err(zip(format!("deflate size mismatch for {}", member.name)));
			}
			Ok(out)
		}
		method => Err(zip(format!(
			"unsupported compression method {method} for {}",
			member.name
		))),
	}
}

fn entry_path(name: &str) -> Result<String, GithubError> {
	store::safe_relative(name).map_err(|error| zip(error.to_string()))
}

fn decode_name(bytes: &[u8], flags: u16) -> Result<String, GithubError> {
	if flags & (1 << 11) == 0 && !bytes.is_ascii() {
		return Err(zip("release entry name is not ASCII"));
	}
	std::str::from_utf8(bytes)
		.map(str::to_string)
		.map_err(|_| zip("release entry name is not UTF-8"))
}

fn find_eocd(bytes: &[u8]) -> Result<usize, GithubError> {
	if bytes.len() < 22 {
		return Err(zip("not a zip archive"));
	}
	let start = bytes.len().saturating_sub(22 + 65535);
	let mut offset = bytes.len() - 22;
	loop {
		if bytes[offset..offset + 4] == 0x0605_4b50u32.to_le_bytes() {
			let comment = read_u16(bytes, offset + 20)? as usize;
			if offset + 22 + comment == bytes.len() {
				return Ok(offset);
			}
		}
		if offset == start {
			break;
		}
		offset -= 1;
	}
	Err(zip(
		"zip comment or end-of-central-directory record is missing",
	))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, GithubError> {
	let slice = bytes
		.get(offset..offset + 2)
		.ok_or_else(|| zip("zip record is truncated"))?;
	Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, GithubError> {
	let slice = bytes
		.get(offset..offset + 4)
		.ok_or_else(|| zip("zip record is truncated"))?;
	Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn crc32(data: &[u8]) -> u32 {
	let mut crc = 0xffff_ffffu32;
	for &byte in data {
		crc ^= u32::from(byte);
		for _ in 0..8 {
			let mask = (crc & 1).wrapping_neg();
			crc = (crc >> 1) ^ (0xedb8_8320 & mask);
		}
	}
	!crc
}

fn zip(message: impl Into<String>) -> GithubError {
	GithubError::Zip(message.into())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn install_specs_split_into_repo_and_reference() {
		for (spec, repo, reference) in [
			(
				"Acture/eu4-dynamic-trade-blackhole@v0.1.0",
				"Acture/eu4-dynamic-trade-blackhole",
				Some("v0.1.0"),
			),
			(
				"Acture/eu4-dynamic-trade-blackhole@latest",
				"Acture/eu4-dynamic-trade-blackhole",
				Some("latest"),
			),
			(
				"Acture/eu4-dynamic-trade-blackhole",
				"Acture/eu4-dynamic-trade-blackhole",
				None,
			),
			(
				"https://github.com/Acture/eu4-dynamic-trade-blackhole.git@v0.2.0",
				"Acture/eu4-dynamic-trade-blackhole",
				Some("v0.2.0"),
			),
		] {
			let (resolved, tag) = split_install_spec(spec).unwrap();
			assert_eq!(resolved, repo, "{spec}");
			assert_eq!(tag.as_deref(), reference, "{spec}");
		}
		assert!(split_install_spec("not-a-repo").is_err());
		assert!(split_install_spec("github.com/owner/repo/extra").is_err());
	}

	use flate2::Compression;
	use flate2::write::DeflateEncoder;
	use std::collections::BTreeMap;
	use std::io::Write;

	fn amd64_dll() -> Vec<u8> {
		pe(store::MACHINE_AMD64)
	}

	fn pe(machine: u16) -> Vec<u8> {
		let mut dll = vec![0; 0x80];
		dll[..2].copy_from_slice(b"MZ");
		dll[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
		dll[0x40..0x44].copy_from_slice(b"PE\0\0");
		dll[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
		dll
	}

	fn deflate(data: &[u8]) -> Vec<u8> {
		let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
		encoder.write_all(data).unwrap();
		encoder.finish().unwrap()
	}

	struct ZipFile {
		name: &'static str,
		data: Vec<u8>,
		method: u16,
		descriptor: bool,
	}

	fn zip_archive(files: &[ZipFile]) -> Vec<u8> {
		let mut local = Vec::new();
		let mut members = Vec::new();
		for file in files {
			let compressed = if file.method == 8 {
				deflate(&file.data)
			} else {
				file.data.clone()
			};
			let offset = local.len() as u32;
			let flags: u16 = if file.descriptor { 8 } else { 0 };
			local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
			local.extend_from_slice(&20u16.to_le_bytes());
			local.extend_from_slice(&flags.to_le_bytes());
			local.extend_from_slice(&file.method.to_le_bytes());
			local.extend_from_slice(&[0; 8]);
			if file.descriptor {
				local.extend_from_slice(&[0; 8]);
			} else {
				local.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
				local.extend_from_slice(&(file.data.len() as u32).to_le_bytes());
			}
			local.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
			local.extend_from_slice(&0u16.to_le_bytes());
			local.extend_from_slice(file.name.as_bytes());
			local.extend_from_slice(&compressed);
			members.push((file, compressed, offset, flags));
		}
		let cd_offset = local.len() as u32;
		let mut cd = Vec::new();
		for (file, compressed, offset, flags) in &members {
			cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
			cd.extend_from_slice(&20u16.to_le_bytes());
			cd.extend_from_slice(&20u16.to_le_bytes());
			cd.extend_from_slice(&flags.to_le_bytes());
			cd.extend_from_slice(&file.method.to_le_bytes());
			cd.extend_from_slice(&[0; 4]);
			cd.extend_from_slice(&crc32(&file.data).to_le_bytes());
			cd.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
			cd.extend_from_slice(&(file.data.len() as u32).to_le_bytes());
			cd.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&[0; 8]);
			cd.extend_from_slice(&offset.to_le_bytes());
			cd.extend_from_slice(file.name.as_bytes());
		}
		let mut out = local;
		out.extend_from_slice(&cd);
		out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out.extend_from_slice(&(members.len() as u16).to_le_bytes());
		out.extend_from_slice(&(members.len() as u16).to_le_bytes());
		out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
		out.extend_from_slice(&cd_offset.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out
	}

	fn file(name: &'static str, data: Vec<u8>, method: u16) -> ZipFile {
		ZipFile {
			name,
			data,
			method,
			descriptor: false,
		}
	}

	#[test]
	fn resolves_owner_repo_url_and_plugin_id() {
		for spec in [
			"YozoraTempest/EU4-Unicode-Patch",
			"yozoratempest/eu4-unicode-patch",
			"https://github.com/YozoraTempest/EU4-Unicode-Patch",
			"https://github.com/YozoraTempest/EU4-Unicode-Patch.git",
			"https://github.com/YozoraTempest/EU4-Unicode-Patch/releases/tag/v0.1.15",
			"git@github.com:YozoraTempest/EU4-Unicode-Patch.git",
			"io.github.yozoratempest.eu4-unicode-patch",
		] {
			let release = resolve(spec).unwrap();
			assert_eq!(release.tag, "v0.1.15", "{spec}");
			assert_eq!(release.asset, "EU4UnicodePatch-1.37.5-v0.1.15.zip");
		}
		assert_eq!(
			resolve("https://github.com/matanki-saito/EU4dll/releases/tag/93")
				.unwrap()
				.adapter,
			"io.github.matanki-saito.eu4dll"
		);
		assert_eq!(
			resolve("YozoraTempest/EU4-Menu-Patch").unwrap().tag,
			"v0.1.4-experimental"
		);
		let mismatch =
			resolve("https://github.com/YozoraTempest/EU4-Unicode-Patch/releases/tag/v0.1.14")
				.unwrap_err();
		assert!(
			matches!(mismatch, GithubError::TagMismatch { .. }),
			"{mismatch}"
		);
		assert!(resolve("not-a-plugin").is_err());
		assert!(resolve("someone/EU4dll").is_err());
	}

	#[test]
	fn download_args_pin_the_release_asset() {
		let release = resolve("matanki-saito/EU4dll").unwrap();
		let args = download_args(
			&release.repo,
			&release.tag,
			&release.asset,
			Path::new("dest"),
		);
		let text: Vec<String> = args
			.iter()
			.map(|arg| arg.to_string_lossy().into_owned())
			.collect();
		assert_eq!(
			text,
			vec![
				"release",
				"download",
				"93",
				"--repo",
				"matanki-saito/EU4dll",
				"--pattern",
				"steam_EU4_fixdll_build_93.zip",
				"--dir",
				"dest",
				"--clobber",
			]
		);
	}

	#[test]
	fn extracts_stored_and_deflated_entries_and_rejects_traversal() {
		let payload = b"hello deflate hello deflate".to_vec();
		let archive = zip_archive(&[
			file("plugins/", Vec::new(), 0),
			file("plugins/readme.txt", b"stored".to_vec(), 0),
			file("plugins/note.txt", payload.clone(), 8),
		]);
		let entries = extract_zip(&archive).unwrap();
		let by_path: BTreeMap<_, _> = entries
			.iter()
			.map(|entry| (entry.path.as_str(), entry.data.as_slice()))
			.collect();
		assert_eq!(by_path["plugins/readme.txt"], b"stored");
		assert_eq!(by_path["plugins/note.txt"], payload.as_slice());
		assert!(!by_path.contains_key("plugins/"));

		let mut descriptor = file("plugins/note.txt", payload.clone(), 8);
		descriptor.descriptor = true;
		let entries = extract_zip(&zip_archive(&[descriptor])).unwrap();
		assert_eq!(entries[0].data, payload);

		let escaped = zip_archive(&[file("../outside.txt", b"no".to_vec(), 0)]);
		assert!(extract_zip(&escaped).is_err());
	}

	#[test]
	fn eu4dll_zip_omits_the_x86_plugin_and_updaters_before_binding() {
		let release = resolve("io.github.matanki-saito.eu4dll").unwrap();
		let archive = zip_archive(&[
			file("plugins/plugin64.dll", amd64_dll(), 0),
			file("plugins/Plugin.dll", pe(0x014c), 0),
			file("plugins/dllautoupdater.exe", b"updater".to_vec(), 0),
			file("plugins/autoupdate64.bat", b"update".to_vec(), 0),
			file("plugins/autoupdate.bat", b"update".to_vec(), 0),
			file("version.dll", amd64_dll(), 0),
			file("d3d9.dll", amd64_dll(), 0),
			file(
				"plugins/plugin.ini",
				b"[options]\nERROR_TEST=no\n".to_vec(),
				0,
			),
			file("README.md", b"readme".to_vec(), 0),
		]);
		let package = package_from_zip(&archive, release).unwrap();
		let paths: BTreeSet<_> = package
			.entries()
			.iter()
			.map(|entry| entry.path.as_str())
			.collect();
		assert!(paths.contains("plugins/plugin64.dll"));
		assert!(paths.contains("plugins/plugin.ini"));
		assert!(paths.contains("README.md"));
		assert!(paths.contains("foch-plugin.toml"));
		for omitted in [
			"plugins/Plugin.dll",
			"plugins/dllautoupdater.exe",
			"plugins/autoupdate64.bat",
			"plugins/autoupdate.bat",
			"version.dll",
			"d3d9.dll",
		] {
			assert!(!paths.contains(omitted), "{omitted} was stored");
		}
		assert_eq!(package.manifest.plugin.version.to_string(), "93.0.0");
		assert_eq!(package.manifest.exe_relative, ["plugins/plugin.ini"]);
		let entry = package
			.entries()
			.iter()
			.find(|entry| entry.path == "plugins/plugin64.dll")
			.unwrap();
		assert_eq!(store::pe_machine(&entry.data), Some(store::MACHINE_AMD64));
	}

	#[test]
	fn unicode_zip_strips_the_proxy_and_pins_0_1_15() {
		let release = resolve("YozoraTempest/EU4-Unicode-Patch").unwrap();
		let archive = zip_archive(&[
			file("plugins/eu4_unicode_patch.dll", amd64_dll(), 8),
			file("VERSION.dll", amd64_dll(), 0),
			file("plugins/eu4_unicode_patch/LICENSE.txt", b"MIT".to_vec(), 0),
		]);
		let package = package_from_zip(&archive, release).unwrap();
		assert_eq!(package.manifest.plugin.version.to_string(), "0.1.15");
		assert!(
			package
				.entries()
				.iter()
				.all(|entry| entry.path != "VERSION.dll")
		);
		assert_eq!(
			package.manifest.status.as_ref().unwrap().export,
			"Eu4UnicodeProbeEnabled"
		);
	}
}
