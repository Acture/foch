//! Persistent cache for full playset merge results.
//!
//! A modset entry stores the materialized output directory as a tar.gz plus
//! the final merge report JSON. The key is computed by callers from the full
//! playset content identity: sorted per-mod content hashes, resolution-map
//! bytes hash, foch version, and game version.

use super::mod_parse_cache::{CacheError, default_foch_cache_dir};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use foch_core::model::MergeReport;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::{Archive, Builder};
use walkdir::WalkDir;

const CACHE_ENV: &str = "FOCH_MODSET_CACHE_DIR";
const MODSETS_DIR_NAME: &str = "modsets";
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ModsetCache {
	root: PathBuf,
	entries_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CachedModsetResult {
	pub tarball_path: PathBuf,
	pub report: MergeReport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEntryInfo {
	pub key: String,
	pub tarball_path: PathBuf,
	pub report_path: PathBuf,
	pub size_bytes: u64,
	pub modified: SystemTime,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
	pub entry_count: usize,
	pub total_bytes: u64,
	pub oldest_mtime: Option<SystemTime>,
	pub newest_mtime: Option<SystemTime>,
}

impl ModsetCache {
	pub fn open(cache_dir: &Path) -> Self {
		let entries_dir = cache_dir.join(MODSETS_DIR_NAME);
		let cache = Self {
			root: cache_dir.to_path_buf(),
			entries_dir,
		};
		let _ = fs::create_dir_all(cache.entries_dir());
		cache
	}

	pub fn open_default() -> Self {
		Self::open(&default_modset_cache_root_dir())
	}

	pub fn open_versioned(cache_dir: &Path, version: &str) -> Result<Self, CacheError> {
		let root = cache_dir.to_path_buf();
		let all_entries_dir = root.join(MODSETS_DIR_NAME);
		fs::create_dir_all(&all_entries_dir).map_err(CacheError::Io)?;
		let namespace = cache_version_namespace(version)?;
		let entries_dir = all_entries_dir.join(&namespace);
		fs::create_dir_all(&entries_dir).map_err(CacheError::Io)?;

		let cleanup = remove_obsolete_version_entries(&all_entries_dir, &namespace)?;
		if cleanup.removed_items > 0 {
			tracing::info!(
				cache_version = version,
				removed_items = cleanup.removed_items,
				freed_bytes = cleanup.freed_bytes,
				"removed obsolete modset cache versions"
			);
		}

		Ok(Self { root, entries_dir })
	}

	pub fn open_default_versioned(version: &str) -> Result<Self, CacheError> {
		Self::open_versioned(&default_modset_cache_root_dir(), version)
	}

	pub fn root(&self) -> &Path {
		&self.root
	}

	pub fn entries_dir(&self) -> PathBuf {
		self.entries_dir.clone()
	}

	pub fn lookup(&self, key: &str) -> Option<CachedModsetResult> {
		let tarball_path = self.tarball_path(key);
		if !tarball_path.is_file() {
			return None;
		}
		let report_path = self.report_path(key);
		let raw = fs::read(&report_path).ok()?;
		let report = serde_json::from_slice::<MergeReport>(&raw).ok()?;
		Some(CachedModsetResult {
			tarball_path,
			report,
		})
	}

	pub fn store(&self, key: &str, out_dir: &Path, report: &MergeReport) -> Result<(), CacheError> {
		fs::create_dir_all(self.entries_dir()).map_err(CacheError::Io)?;

		let tarball_path = self.tarball_path(key);
		let report_path = self.report_path(key);
		let temp_suffix = cache_temp_suffix();
		let tmp_tarball = self
			.entries_dir()
			.join(format!("{key}.{temp_suffix}.tar.gz.tmp"));
		let tmp_report = self
			.entries_dir()
			.join(format!("{key}.{temp_suffix}.report.json.tmp"));

		let report_bytes =
			serde_json::to_vec_pretty(report).map_err(|err| CacheError::Encode(err.to_string()))?;
		fs::write(&tmp_report, report_bytes).map_err(CacheError::Io)?;

		let tarball = fs::File::create(&tmp_tarball).map_err(CacheError::Io)?;
		let encoder = GzEncoder::new(tarball, Compression::default());
		let mut builder = Builder::new(encoder);
		builder.follow_symlinks(false);
		builder
			.append_dir_all(".", out_dir)
			.map_err(CacheError::Io)?;
		let encoder = builder.into_inner().map_err(CacheError::Io)?;
		encoder.finish().map_err(CacheError::Io)?;

		fs::rename(&tmp_tarball, &tarball_path).map_err(|err| {
			let _ = fs::remove_file(&tmp_tarball);
			let _ = fs::remove_file(&tmp_report);
			CacheError::Io(err)
		})?;
		fs::rename(&tmp_report, &report_path).map_err(|err| {
			let _ = fs::remove_file(&tmp_report);
			CacheError::Io(err)
		})?;
		Ok(())
	}

	pub fn purge_older_than(&self, days: u32) -> Result<usize, CacheError> {
		let cutoff = cutoff_for_days(days);
		let mut purged = 0_usize;
		for entry in self.list_entries()? {
			if entry.modified >= cutoff {
				continue;
			}
			remove_if_exists(&entry.tarball_path)?;
			remove_if_exists(&entry.report_path)?;
			purged += 1;
		}
		prune_empty_dirs(&self.entries_dir());
		Ok(purged)
	}

	pub fn list_entries(&self) -> Result<Vec<CacheEntryInfo>, CacheError> {
		let mut entries = Vec::new();
		let root = self.entries_dir();
		if !root.is_dir() {
			return Ok(entries);
		}
		let mut tarballs = Vec::new();
		collect_tarballs(&root, &mut tarballs)?;
		for path in tarballs {
			let Some(key) = tarball_key(&path) else {
				continue;
			};
			let report_path = path
				.parent()
				.unwrap_or(&root)
				.join(format!("{key}.report.json"));
			if !report_path.is_file() {
				continue;
			}
			let tar_meta = fs::metadata(&path).map_err(CacheError::Io)?;
			let report_meta = fs::metadata(&report_path).map_err(CacheError::Io)?;
			let tar_mtime = tar_meta.modified().unwrap_or(UNIX_EPOCH);
			let report_mtime = report_meta.modified().unwrap_or(UNIX_EPOCH);
			entries.push(CacheEntryInfo {
				key,
				tarball_path: path,
				report_path,
				size_bytes: tar_meta.len().saturating_add(report_meta.len()),
				modified: tar_mtime.max(report_mtime),
			});
		}
		entries.sort_by(|left, right| {
			left.modified
				.cmp(&right.modified)
				.then_with(|| left.key.cmp(&right.key))
				.then_with(|| left.tarball_path.cmp(&right.tarball_path))
		});
		Ok(entries)
	}

	pub fn stats(&self) -> Result<CacheStats, CacheError> {
		let entries = self.list_entries()?;
		Ok(stats_from_entries(&entries))
	}

	fn tarball_path(&self, key: &str) -> PathBuf {
		self.entries_dir().join(format!("{key}.tar.gz"))
	}

	fn report_path(&self, key: &str) -> PathBuf {
		self.entries_dir().join(format!("{key}.report.json"))
	}
}

pub fn default_modset_cache_root_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ENV) {
		return PathBuf::from(override_dir);
	}
	default_foch_cache_dir()
}

pub fn default_modset_cache_dir() -> PathBuf {
	default_modset_cache_root_dir().join(MODSETS_DIR_NAME)
}

fn cache_version_namespace(version: &str) -> Result<String, CacheError> {
	if version.is_empty()
		|| !version
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
	{
		return Err(CacheError::Io(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("invalid modset cache version: {version:?}"),
		)));
	}
	Ok(format!("v{version}"))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct VersionCleanupStats {
	removed_items: usize,
	freed_bytes: u64,
}

fn remove_obsolete_version_entries(
	entries_root: &Path,
	active_namespace: &str,
) -> Result<VersionCleanupStats, CacheError> {
	let mut stats = VersionCleanupStats::default();
	for entry in fs::read_dir(entries_root).map_err(CacheError::Io)? {
		let entry = entry.map_err(CacheError::Io)?;
		if entry.file_name() == active_namespace {
			continue;
		}
		let path = entry.path();
		stats.freed_bytes = stats.freed_bytes.saturating_add(path_size(&path)?);
		if entry.file_type().map_err(CacheError::Io)?.is_dir() {
			fs::remove_dir_all(&path).map_err(CacheError::Io)?;
		} else {
			remove_if_exists(&path)?;
		}
		stats.removed_items += 1;
	}
	Ok(stats)
}

fn path_size(path: &Path) -> Result<u64, CacheError> {
	if !path.is_dir() {
		return fs::metadata(path)
			.map(|metadata| metadata.len())
			.map_err(CacheError::Io);
	}
	let mut bytes = 0_u64;
	for entry in WalkDir::new(path).min_depth(1) {
		let entry = entry.map_err(|err| {
			CacheError::Io(
				err.into_io_error()
					.unwrap_or_else(|| io::Error::other("failed to walk modset cache")),
			)
		})?;
		if entry.file_type().is_file() {
			bytes = bytes.saturating_add(fs::metadata(entry.path()).map_err(CacheError::Io)?.len());
		}
	}
	Ok(bytes)
}

fn collect_tarballs(root: &Path, tarballs: &mut Vec<PathBuf>) -> Result<(), CacheError> {
	for entry in fs::read_dir(root).map_err(CacheError::Io)? {
		let entry = entry.map_err(CacheError::Io)?;
		let file_type = entry.file_type().map_err(CacheError::Io)?;
		let path = entry.path();
		if file_type.is_dir() {
			collect_tarballs(&path, tarballs)?;
		} else if file_type.is_file() && is_tar_gz_path(&path) {
			tarballs.push(path);
		}
	}
	Ok(())
}

pub fn compute_resolution_map_hash(config_bytes: &[u8]) -> String {
	blake3::hash(config_bytes).to_hex().to_string()
}

pub fn compute_modset_cache_key(
	mod_hashes: &[String],
	resolution_map_hash: &str,
	foch_version: &str,
	game_version: &str,
) -> String {
	let mut sorted_mod_hashes = mod_hashes.to_vec();
	sorted_mod_hashes.sort();

	let mut hasher = blake3::Hasher::new();
	for mod_hash in sorted_mod_hashes {
		update_hash_part(&mut hasher, mod_hash.as_bytes());
	}
	update_hash_part(&mut hasher, resolution_map_hash.as_bytes());
	update_hash_part(&mut hasher, foch_version.as_bytes());
	update_hash_part(&mut hasher, game_version.as_bytes());
	hasher.finalize().to_hex().to_string()
}

pub fn unpack_modset_tarball(tarball_path: &Path, out_dir: &Path) -> Result<(), CacheError> {
	fs::create_dir_all(out_dir).map_err(CacheError::Io)?;
	let file = fs::File::open(tarball_path).map_err(CacheError::Io)?;
	let decoder = GzDecoder::new(file);
	let mut archive = Archive::new(decoder);
	for entry in archive.entries().map_err(CacheError::Io)? {
		let mut entry = entry.map_err(CacheError::Io)?;
		let entry_path = entry.path().map_err(CacheError::Io)?.into_owned();
		let rel_path = validate_tar_entry_path(&entry_path)?;
		validate_tar_link_target(&mut entry, &entry_path, &rel_path)?;
		if !entry.unpack_in(out_dir).map_err(CacheError::Io)? {
			return Err(invalid_tar_entry(format!(
				"tarball entry path escapes output directory: {}",
				entry_path.display()
			)));
		}
	}
	Ok(())
}

fn validate_tar_entry_path(path: &Path) -> Result<PathBuf, CacheError> {
	if has_windows_absolute_prefix(path) {
		return Err(invalid_tar_entry(format!(
			"tarball entry path is absolute: {}",
			path.display()
		)));
	}
	let mut rel_path = PathBuf::new();
	for component in path.components() {
		match component {
			Component::CurDir => {}
			Component::Normal(part) => rel_path.push(part),
			Component::ParentDir => {
				return Err(invalid_tar_entry(format!(
					"tarball entry path contains parent traversal: {}",
					path.display()
				)));
			}
			Component::RootDir | Component::Prefix(_) => {
				return Err(invalid_tar_entry(format!(
					"tarball entry path is absolute: {}",
					path.display()
				)));
			}
		}
	}
	Ok(rel_path)
}

fn validate_tar_link_target<R: io::Read>(
	entry: &mut tar::Entry<'_, R>,
	entry_path: &Path,
	rel_path: &Path,
) -> Result<(), CacheError> {
	let entry_type = entry.header().entry_type();
	if !(entry_type.is_symlink() || entry_type.is_hard_link()) {
		return Ok(());
	}
	let link_name = entry.link_name().map_err(CacheError::Io)?.ok_or_else(|| {
		invalid_tar_entry(format!(
			"tarball link entry has no target: {}",
			entry_path.display()
		))
	})?;
	if link_name.as_os_str().is_empty() || has_windows_absolute_prefix(&link_name) {
		return Err(invalid_tar_entry(format!(
			"tarball link target escapes output directory: {} -> {}",
			entry_path.display(),
			link_name.display()
		)));
	}

	let mut normalized = rel_path
		.parent()
		.map(path_normal_components)
		.unwrap_or_default();
	for component in link_name.components() {
		match component {
			Component::CurDir => {}
			Component::Normal(part) => normalized.push(part.to_os_string()),
			Component::ParentDir => {
				if normalized.pop().is_none() {
					return Err(invalid_tar_entry(format!(
						"tarball link target escapes output directory: {} -> {}",
						entry_path.display(),
						link_name.display()
					)));
				}
			}
			Component::RootDir | Component::Prefix(_) => {
				return Err(invalid_tar_entry(format!(
					"tarball link target escapes output directory: {} -> {}",
					entry_path.display(),
					link_name.display()
				)));
			}
		}
	}
	Ok(())
}

fn path_normal_components(path: &Path) -> Vec<std::ffi::OsString> {
	path.components()
		.filter_map(|component| match component {
			Component::Normal(part) => Some(part.to_os_string()),
			_ => None,
		})
		.collect()
}

fn has_windows_absolute_prefix(path: &Path) -> bool {
	let text = path.to_string_lossy();
	let bytes = text.as_bytes();
	text.starts_with('\\')
		|| bytes.get(0..3).is_some_and(|prefix| {
			prefix[0].is_ascii_alphabetic()
				&& prefix[1] == b':'
				&& matches!(prefix[2], b'/' | b'\\')
		})
}

fn invalid_tar_entry(message: String) -> CacheError {
	CacheError::Io(io::Error::new(io::ErrorKind::InvalidData, message))
}

fn stats_from_entries(entries: &[CacheEntryInfo]) -> CacheStats {
	let mut stats = CacheStats {
		entry_count: entries.len(),
		total_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
		oldest_mtime: None,
		newest_mtime: None,
	};
	for entry in entries {
		stats.oldest_mtime = Some(match stats.oldest_mtime {
			Some(current) if current <= entry.modified => current,
			_ => entry.modified,
		});
		stats.newest_mtime = Some(match stats.newest_mtime {
			Some(current) if current >= entry.modified => current,
			_ => entry.modified,
		});
	}
	stats
}

fn cutoff_for_days(days: u32) -> SystemTime {
	SystemTime::now()
		.checked_sub(Duration::from_secs(days as u64 * 24 * 60 * 60))
		.unwrap_or(UNIX_EPOCH)
}

fn remove_if_exists(path: &Path) -> Result<(), CacheError> {
	match fs::remove_file(path) {
		Ok(()) => Ok(()),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(err) => Err(CacheError::Io(err)),
	}
}

fn is_tar_gz_path(path: &Path) -> bool {
	path.file_name()
		.and_then(|name| name.to_str())
		.is_some_and(|name| name.ends_with(".tar.gz"))
}

fn tarball_key(path: &Path) -> Option<String> {
	let name = path.file_name()?.to_str()?;
	Some(name.strip_suffix(".tar.gz")?.to_string())
}

fn update_hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

fn cache_temp_suffix() -> String {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let counter = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
	format!("{}.{}.{}", std::process::id(), nanos, counter)
}

fn prune_empty_dirs(root: &Path) {
	if !root.is_dir() {
		return;
	}
	let dirs = WalkDir::new(root)
		.min_depth(1)
		.contents_first(true)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_dir())
		.map(|entry| entry.into_path())
		.collect::<Vec<_>>();
	for dir in dirs {
		let _ = fs::remove_dir(&dir);
	}
}

#[cfg(test)]
mod tests {
	use super::super::mod_parse_cache::compute_mod_hash;
	use super::*;
	use filetime::{FileTime, set_file_mtime};
	use foch_core::model::{MergeReportStatus, MergeReportValidation};
	use std::sync::atomic::{AtomicUsize, Ordering};
	use tempfile::TempDir;

	static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

	fn cache_root(name: &str) -> PathBuf {
		let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.and_then(Path::parent)
			.expect("repo root");
		let path = repo_root.join("target").join("test-cache").join(format!(
			"{name}-{}-{}",
			std::process::id(),
			TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		let _ = fs::remove_dir_all(&path);
		fs::create_dir_all(&path).expect("create cache root");
		path
	}

	fn sample_report() -> MergeReport {
		MergeReport {
			status: MergeReportStatus::Ready,
			generated_file_count: 1,
			validation: MergeReportValidation {
				fatal_errors: 0,
				strict_findings: 1,
				advisory_findings: 2,
				parse_errors: 0,
				unresolved_references: 0,
				missing_localisation: 0,
			},
			..MergeReport::default()
		}
	}

	fn write_out_dir(content: &str) -> TempDir {
		let out = TempDir::new_in(
			Path::new(env!("CARGO_MANIFEST_DIR"))
				.parent()
				.and_then(Path::parent)
				.expect("repo root")
				.join("target"),
		)
		.expect("temp out dir");
		let file = out
			.path()
			.join("common")
			.join("scripted_effects")
			.join("x.txt");
		fs::create_dir_all(file.parent().expect("parent")).expect("create parent");
		fs::write(file, content).expect("write output file");
		out
	}

	fn write_mod_file(mod_root: &Path, content: &str) {
		let file = mod_root
			.join("common")
			.join("scripted_effects")
			.join("x.txt");
		fs::create_dir_all(file.parent().expect("parent")).expect("create mod parent");
		fs::write(file, content).expect("write mod file");
	}

	fn write_tarball_entry(tarball_path: &Path, entry_path: &str, content: &[u8]) {
		let tarball = fs::File::create(tarball_path).expect("create tarball");
		let encoder = GzEncoder::new(tarball, Compression::default());
		let mut builder = Builder::new(encoder);
		let mut header = tar::Header::new_ustar();
		set_raw_tar_path(&mut header, entry_path);
		header.set_size(content.len() as u64);
		header.set_mode(0o644);
		header.set_cksum();
		builder.append(&header, content).expect("append tar entry");
		let encoder = builder.into_inner().expect("finish tar");
		encoder.finish().expect("finish gzip");
	}

	fn set_raw_tar_path(header: &mut tar::Header, entry_path: &str) {
		let name_len = header.as_old().name.len();
		let prefix_len = header.as_ustar().expect("ustar header").prefix.len();
		let path = entry_path.as_bytes();
		let split = if path.len() <= name_len {
			None
		} else {
			entry_path
				.rfind('/')
				.filter(|index| *index <= prefix_len && entry_path.len() - index - 1 <= name_len)
		};

		match split {
			Some(index) => {
				let (prefix, name_with_sep) = entry_path.split_at(index);
				let name = &name_with_sep[1..];
				let ustar = header.as_ustar_mut().expect("ustar header");
				ustar.name.fill(0);
				ustar.prefix.fill(0);
				ustar.name[..name.len()].copy_from_slice(name.as_bytes());
				ustar.prefix[..prefix.len()].copy_from_slice(prefix.as_bytes());
			}
			None => {
				assert!(path.len() <= name_len, "entry path fits ustar header");
				let old = header.as_old_mut();
				old.name.fill(0);
				old.name[..path.len()].copy_from_slice(path);
			}
		}
	}

	fn modset_key(
		mod_hashes: &[String],
		resolution_bytes: &[u8],
		foch_version: &str,
		game_version: &str,
	) -> String {
		compute_modset_cache_key(
			mod_hashes,
			&compute_resolution_map_hash(resolution_bytes),
			foch_version,
			game_version,
		)
	}

	#[test]
	fn modset_cache_lookup_miss_then_store_then_hit() {
		let cache = ModsetCache::open(&cache_root("modset-roundtrip"));
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();

		assert!(cache.lookup("key-a").is_none());
		cache
			.store("key-a", out.path(), &report)
			.expect("store result");
		let hit = cache.lookup("key-a").expect("cache hit");

		assert!(hit.tarball_path.is_file());
		assert_eq!(hit.report.generated_file_count, 1);
		assert_eq!(hit.report.validation.strict_findings, 1);

		let restore = write_out_dir("stale\n");
		fs::remove_dir_all(restore.path()).expect("clear restore dir");
		unpack_modset_tarball(&hit.tarball_path, restore.path()).expect("unpack tarball");
		let restored = fs::read_to_string(
			restore
				.path()
				.join("common")
				.join("scripted_effects")
				.join("x.txt"),
		)
		.expect("read restored output");
		assert_eq!(restored, "effect = { add_prestige = 1 }\n");
	}

	#[test]
	fn extract_rejects_parent_traversal_entry() {
		let root = cache_root("modset-tar-parent-traversal");
		let tarball = root.join("malicious.tar.gz");
		write_tarball_entry(&tarball, "../../escape.txt", b"escape\n");
		let out_dir = root.join("nested").join("out");

		let err = unpack_modset_tarball(&tarball, &out_dir).expect_err("reject traversal");

		assert!(err.to_string().contains("parent traversal"));
		assert!(!root.join("escape.txt").exists());
		assert!(!out_dir.join("escape.txt").exists());
	}

	#[test]
	fn extract_rejects_absolute_path_entry() {
		let root = cache_root("modset-tar-absolute");
		let tarball = root.join("malicious.tar.gz");
		let escape = root.join("absolute_escape.txt");
		write_tarball_entry(&tarball, &escape.to_string_lossy(), b"escape\n");
		let out_dir = root.join("out");

		let err = unpack_modset_tarball(&tarball, &out_dir).expect_err("reject absolute path");

		assert!(err.to_string().contains("absolute"));
		assert!(!escape.exists());
	}

	#[test]
	fn extract_accepts_normal_entry() {
		let root = cache_root("modset-tar-normal");
		let tarball = root.join("normal.tar.gz");
		write_tarball_entry(
			&tarball,
			"common/scripted_effects/x.txt",
			b"effect = { add_prestige = 1 }\n",
		);
		let out_dir = root.join("out");

		unpack_modset_tarball(&tarball, &out_dir).expect("unpack normal entry");

		assert_eq!(
			fs::read_to_string(
				out_dir
					.join("common")
					.join("scripted_effects")
					.join("x.txt")
			)
			.expect("read extracted file"),
			"effect = { add_prestige = 1 }\n"
		);
	}

	#[test]
	fn modset_cache_invalidates_when_resolution_map_changes() {
		let cache = ModsetCache::open(&cache_root("modset-resolution-change"));
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();
		let mods = vec!["mod-a".to_string(), "mod-b".to_string()];
		let first = modset_key(
			&mods,
			b"[[resolutions]]\nprefer_mod = 'a'\n",
			"0.1.0",
			"eu4 1.37",
		);
		let second = modset_key(
			&mods,
			b"[[resolutions]]\nprefer_mod = 'b'\n",
			"0.1.0",
			"eu4 1.37",
		);

		cache
			.store(&first, out.path(), &report)
			.expect("store first");

		assert!(cache.lookup(&first).is_some());
		assert!(cache.lookup(&second).is_none());
	}

	#[test]
	fn modset_cache_invalidates_when_mod_hash_changes() {
		let cache = ModsetCache::open(&cache_root("modset-mod-change"));
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();
		let mod_root = cache_root("mod-content");
		write_mod_file(&mod_root, "effect = { add_prestige = 1 }\n");
		let first_hash = compute_mod_hash(&mod_root).expect("first mod hash");
		write_mod_file(&mod_root, "effect = { add_prestige = 2 }\n");
		let second_hash = compute_mod_hash(&mod_root).expect("second mod hash");
		let first = modset_key(&[first_hash], b"", "0.1.0", "eu4 1.37");
		let second = modset_key(&[second_hash], b"", "0.1.0", "eu4 1.37");

		cache
			.store(&first, out.path(), &report)
			.expect("store first");

		assert!(cache.lookup(&first).is_some());
		assert!(cache.lookup(&second).is_none());
	}

	#[test]
	fn modset_cache_invalidates_when_foch_version_changes() {
		let cache = ModsetCache::open(&cache_root("modset-version-change"));
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();
		let mods = vec!["mod-a".to_string()];
		let first = modset_key(&mods, b"", "0.1.0", "eu4 1.37");
		let second = modset_key(&mods, b"", "0.2.0", "eu4 1.37");

		cache
			.store(&first, out.path(), &report)
			.expect("store first");

		assert!(cache.lookup(&first).is_some());
		assert!(cache.lookup(&second).is_none());
	}

	#[test]
	fn versioned_cache_removes_legacy_and_obsolete_versions() {
		let root = cache_root("modset-version-lifecycle");
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();
		let legacy = ModsetCache::open(&root);
		legacy
			.store("legacy", out.path(), &report)
			.expect("store legacy entry");

		let old = ModsetCache::open_versioned(&root, "11.4.0").expect("open old version");
		assert!(legacy.lookup("legacy").is_none());
		old.store("old", out.path(), &report)
			.expect("store old version entry");

		let current = ModsetCache::open_versioned(&root, "11.4.1").expect("open current version");
		assert!(old.lookup("old").is_none());
		assert!(!root.join(MODSETS_DIR_NAME).join("v11.4.0").exists());
		current
			.store("current", out.path(), &report)
			.expect("store current version entry");

		let reopened =
			ModsetCache::open_versioned(&root, "11.4.1").expect("reopen current version");
		assert!(reopened.lookup("current").is_some());
		assert_eq!(ModsetCache::open(&root).list_entries().unwrap().len(), 1);
	}

	#[test]
	fn modset_cache_purge_older_than_drops_old_entries() {
		let cache = ModsetCache::open(&cache_root("modset-purge"));
		let out = write_out_dir("effect = { add_prestige = 1 }\n");
		let report = sample_report();
		cache.store("old", out.path(), &report).expect("store old");
		cache.store("new", out.path(), &report).expect("store new");

		let old_time = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 40);
		set_file_mtime(
			cache.tarball_path("old"),
			FileTime::from_system_time(old_time),
		)
		.expect("old tar mtime");
		set_file_mtime(
			cache.report_path("old"),
			FileTime::from_system_time(old_time),
		)
		.expect("old report mtime");

		let purged = cache.purge_older_than(30).expect("purge old entries");

		assert_eq!(purged, 1);
		assert!(cache.lookup("old").is_none());
		assert!(cache.lookup("new").is_some());
	}
}
