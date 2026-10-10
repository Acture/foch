//! Freeze installed artifacts/configuration and stage the host's wire plan.
//! This module never loads a DLL in the management process.

use super::{
	manifest::{ConfigField, Kind, Manifest, Phase, StatusRule},
	planner::{self, GameIdentity, Selection},
	store::{self, ArchiveEntry, ValidatedPackage},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	io::{self, Read},
	path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPlan {
	pub format: u32,
	pub run_id: String,
	pub game_version: String,
	pub events: String,
	pub plugins: Vec<HostPlugin>,
}

/// The host refuses larger frozen plans before loading any plugin.
pub const MAX_PLAN_BYTES: usize = 1024 * 1024;

fn wire_bytes(plan: &HostPlan) -> io::Result<Vec<u8>> {
	plan.validate().map_err(io::Error::other)?;
	let bytes = serde_json::to_vec_pretty(plan)?;
	if bytes.len() > MAX_PLAN_BYTES {
		return Err(io::Error::other("host plan exceeds the 1 MiB limit"));
	}
	Ok(bytes)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPlugin {
	pub id: String,
	pub version: String,
	pub kind: Kind,
	pub phase: Phase,
	pub path: String,
	pub sha256: String,
	pub config_json: String,
	pub dirs: BTreeMap<String, String>,
	pub status: Option<StatusRule>,
	pub abi_major: Option<u32>,
}

impl HostPlan {
	/// Validate the format-1 contract before starting the game. Package
	/// semantics are checked on import/resolve; these are the final wire paths,
	/// identities and phase order the host will actually receive.
	pub fn validate(&self) -> Result<(), String> {
		if self.format != 1
			|| self.run_id.is_empty()
			|| self.run_id.len() > 256
			|| self.run_id.contains('\0')
			|| !absolute_windows_path(&self.events)
		{
			return Err("invalid host plan format, run identity or events path".into());
		}
		let mut ids = BTreeSet::new();
		let mut names = BTreeSet::new();
		let mut phase = Phase::Entry;
		for plugin in &self.plugins {
			let path = plugin.path.replace('/', "\\").to_lowercase();
			let name = path.rsplit('\\').next().unwrap_or_default();
			if plugin.id.is_empty()
				|| plugin.id.len() > 256
				|| plugin.id.contains('\0')
				|| plugin.version.is_empty()
				|| plugin.version.contains('\0')
				|| !ids.insert(&plugin.id)
				|| !absolute_windows_path(&plugin.path)
				|| !path.ends_with(".dll")
				|| !names.insert(name.to_string())
				|| plugin.phase < phase
				|| plugin.phase == Phase::ProcessAttach
				|| (plugin.kind == Kind::Native && plugin.abi_major != Some(1))
				|| plugin.sha256.len() != 64
				|| !plugin.sha256.bytes().all(|b| b.is_ascii_hexdigit())
				|| !serde_json::from_str::<Value>(&plugin.config_json)
					.is_ok_and(|value| value.is_object())
				|| plugin.dirs.iter().any(|(kind, path)| {
					!matches!(kind.as_str(), "data" | "cache" | "log" | "plugin")
						|| !absolute_windows_path(path)
				}) {
				return Err(format!("{}: invalid host plan entry", plugin.id));
			}
			if let Some(status) = &plugin.status {
				if plugin.kind != Kind::Legacy
					|| status.export.is_empty()
					|| !status.export.is_ascii()
					|| status.export.contains('\0')
					|| (status.mode == super::manifest::StatusMode::Poll
						&& (status.timeout_ms == 0
							|| status.timeout_ms > 120_000
							|| status.pending.is_empty()))
				{
					return Err(format!("{}: invalid host status rule", plugin.id));
				}
				let mut values = BTreeSet::new();
				if status.values.iter().any(|value| {
					!values.insert(value.value)
						|| !matches!(
							value.state.as_str(),
							"active" | "inactive" | "refused" | "failed" | "unknown"
						)
				}) {
					return Err(format!("{}: invalid host status mapping", plugin.id));
				}
			}
			phase = plugin.phase;
		}
		Ok(())
	}
}

fn absolute_windows_path(path: &str) -> bool {
	if path.contains('\0') {
		return false;
	}
	let normalized = path.replace('/', "\\");
	let bytes = normalized.as_bytes();
	let drive =
		bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
	let unc = normalized.starts_with(r"\\")
		&& normalized[2..]
			.split('\\')
			.filter(|p| !p.is_empty())
			.count() >= 2;
	(drive || unc)
		&& !normalized
			.split('\\')
			.any(|part| matches!(part, "." | ".." | "?"))
}

struct Package {
	validated: ValidatedPackage,
	config: Value,
}

pub struct Deployment {
	game_version: String,
	packages: Vec<Package>,
	resolution: planner::Resolution,
}

#[derive(Debug, Serialize)]
pub struct Prepared {
	pub plan: HostPlan,
	pub plan_sha256: String,
	pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Serialize)]
pub struct Artifact {
	pub id: String,
	pub version: String,
	pub package_sha256: String,
}

/// Resolve real installed artifacts, refusing missing or ambiguous versions.
/// All bytes are validated and retained before runtime construction begins.
pub fn resolve(
	game: &GameIdentity,
	store_root: &Path,
	selection: &[Selection],
) -> Result<Deployment, String> {
	let (installed, _) = store::installed_versions(store_root);
	let mut catalog = BTreeMap::new();
	let mut packages = BTreeMap::new();
	for choice in selection.iter().filter(|item| item.enabled) {
		if packages.contains_key(&choice.id) {
			return Err(format!("duplicate selection {}", choice.id));
		}
		let matching: Vec<_> = installed
			.iter()
			.filter(|item| {
				item.manifest.plugin.id == choice.id
					&& item.manifest.plugin.version == choice.version
			})
			.collect();
		if matching.len() != 1 {
			return Err(format!(
				"{} {}: expected one installed artifact, found {} (import the selected release or remove ambiguity)",
				choice.id,
				choice.version,
				matching.len()
			));
		}
		let canonical_store = store_root
			.canonicalize()
			.map_err(|error| error.to_string())?;
		let directory = &matching[0].dir;
		// Recheck the plugin/version boundary immediately before freezing bytes.
		store::checked_directory(directory.parent().unwrap(), &canonical_store)
			.map_err(|error| format!("{}: {error}", choice.id))?;
		let directory = store::checked_directory(directory, &canonical_store)
			.map_err(|error| format!("{}: {error}", choice.id))?;
		let entries =
			read_package(&directory).map_err(|error| format!("{}: {error}", choice.id))?;
		let validated =
			store::validate(entries).map_err(|errors| format!("{}: {errors:?}", choice.id))?;
		if directory.file_name().and_then(|name| name.to_str())
			!= Some(validated.version_dir_name().as_str())
		{
			return Err(format!(
				"{}: installed artifact identity changed",
				choice.id
			));
		}
		let mut manifest = validated.manifest.clone();
		// Include all actual dependency DLLs in process-wide basename checks,
		// even when an older package omitted them from its file declaration.
		manifest.files = validated
			.entries()
			.iter()
			.filter(|entry| entry.path != super::manifest::FILE_NAME)
			.map(|entry| super::manifest::FileEntry {
				path: entry.path.clone(),
				sha256: hash(&entry.data),
			})
			.collect();
		let config = effective_config(&manifest, &choice.config)?;
		if manifest.entry.phase == Phase::ProcessAttach {
			return Err(format!(
				"{}: process_attach is not verified by this host",
				choice.id
			));
		}
		catalog
			.entry(choice.id.clone())
			.or_insert_with(BTreeMap::new)
			.insert(choice.version.clone(), manifest);
		packages.insert(choice.id.clone(), Package { validated, config });
	}
	let resolution = planner::plan(game, &catalog, selection);
	if !resolution.is_launchable() {
		return Err(format!(
			"plugin plan is not launchable: {:?}",
			resolution.errors
		));
	}
	let packages = resolution
		.order
		.iter()
		.map(|item| packages.remove(&item.id).unwrap())
		.collect();
	Ok(Deployment {
		game_version: game.version.to_string(),
		packages,
		resolution,
	})
}

fn read_package(root: &Path) -> io::Result<Vec<ArchiveEntry>> {
	let root = root.canonicalize()?;
	fn walk(root: &Path, directory: &Path, entries: &mut Vec<ArchiveEntry>) -> io::Result<()> {
		for item in fs::read_dir(directory)? {
			let item = item?;
			let path = item.path();
			if !path.canonicalize()?.starts_with(root) {
				return Err(io::Error::other(
					"package file escapes the installed artifact",
				));
			}
			if item.file_type()?.is_symlink() {
				return Err(io::Error::other("package links are not supported"));
			}
			#[cfg(windows)]
			{
				use std::os::windows::fs::MetadataExt;
				if fs::symlink_metadata(&path)?.file_attributes() & 0x400 != 0 {
					return Err(io::Error::other("package reparse points are not supported"));
				}
			}
			if item.file_type()?.is_dir() {
				walk(root, &path, entries)?;
			} else {
				entries.push(ArchiveEntry {
					path: path
						.strip_prefix(root)
						.unwrap()
						.to_string_lossy()
						.replace('\\', "/"),
					data: fs::read(&path)?,
				});
			}
		}
		Ok(())
	}
	let mut entries = Vec::new();
	walk(&root, &root, &mut entries)?;
	Ok(entries)
}

pub fn effective_config(manifest: &Manifest, chosen: &Value) -> Result<Value, String> {
	let chosen = chosen.as_object().ok_or_else(|| {
		format!(
			"{}: configuration must be a JSON object",
			manifest.plugin.id
		)
	})?;
	let mut config = manifest.default_config();
	for (name, value) in chosen {
		if !manifest.config.contains_key(name) {
			return Err(format!(
				"{}: unknown configuration field {name}",
				manifest.plugin.id
			));
		}
		config[name] = value.clone();
	}
	for (name, value) in config.as_object().unwrap() {
		let valid = match manifest.config.get(name) {
			Some(ConfigField::Bool { .. }) => value.is_boolean(),
			Some(ConfigField::Int { min, max, .. }) => value.as_i64().is_some_and(|number| {
				min.zip(*max).is_none_or(|(min, max)| min <= max)
					&& min.is_none_or(|min| number >= min)
					&& max.is_none_or(|max| number <= max)
			}),
			Some(ConfigField::String { .. }) => value.is_string(),
			None => false,
		};
		if !valid {
			return Err(format!(
				"{}: invalid configuration field {name}",
				manifest.plugin.id
			));
		}
	}
	Ok(config)
}

impl Deployment {
	/// The plan validated against the actual bytes this deployment retains.
	/// Management previews and launch must use this same result.
	pub fn resolution(&self) -> &planner::Resolution {
		&self.resolution
	}

	/// Stage into a fresh Foch layer. Artifact files are copied, never hard
	/// linked, so legacy log/config writes cannot mutate the version store.
	pub fn stage(
		&self,
		runtime: &Path,
		data_root: &Path,
		run_id: &str,
		game_root: &Path,
	) -> io::Result<Prepared> {
		if !store::ordinary_metadata(runtime)?.is_dir() {
			return Err(io::Error::other(
				"runtime layer is not an ordinary directory",
			));
		}
		let runtime = runtime.canonicalize()?;
		if !store::ordinary_metadata(&runtime.join("foch-runtime"))?.is_file() {
			return Err(io::Error::other("not a prepared Foch runtime layer"));
		}
		ensure_outside_game(game_root, &runtime)?;
		ensure_outside_game(game_root, data_root)?;
		fs::create_dir(runtime.join("plugins"))?;
		fs::create_dir(runtime.join("foch-host"))?;
		fs::create_dir_all(data_root)?;
		let data_root = data_root.canonicalize()?;
		let mut plan = HostPlan {
			format: 1,
			run_id: run_id.into(),
			game_version: self.game_version.clone(),
			events: path_text(&runtime.join("foch-host/events.jsonl"))?,
			plugins: Vec::new(),
		};
		let mut artifacts = Vec::new();
		for package in &self.packages {
			let manifest = &package.validated.manifest;
			let identity = hash(manifest.plugin.id.as_bytes());
			let root = runtime.join("plugins").join(&identity[..12]);
			fs::create_dir(&root)?;
			for file in package.validated.entries() {
				if store::is_proxy_name(file.path.rsplit('/').next().unwrap()) {
					return Err(io::Error::other(
						"plugin packages may not deploy another proxy loader",
					));
				}
				let destination = root.join(&file.path);
				if path_text(&destination)?.encode_utf16().count() >= 260 {
					return Err(io::Error::other(
						"plugin resource exceeds the Windows path budget",
					));
				}
				if let Some(parent) = destination.parent() {
					fs::create_dir_all(parent)?;
				}
				fs::write(destination, &file.data)?;
			}
			write_config(&root, manifest, &package.config)?;
			let path = root.join(&manifest.entry.path);
			let plugin_dir = path.parent().unwrap();
			let state = data_root.join("plugins/state").join(&identity[..12]);
			let cache = data_root
				.join("plugins/cache")
				.join(&package.validated.digest);
			ensure_outside_game(game_root, &state)?;
			ensure_outside_game(game_root, &cache)?;
			fs::create_dir_all(&state)?;
			fs::create_dir_all(&cache)?;
			let dirs = BTreeMap::from([
				("data".into(), path_text(&state)?),
				("cache".into(), path_text(&cache)?),
				("log".into(), path_text(plugin_dir)?),
				("plugin".into(), path_text(plugin_dir)?),
			]);
			plan.plugins.push(HostPlugin {
				id: manifest.plugin.id.clone(),
				version: manifest.plugin.version.to_string(),
				kind: manifest.entry.kind.clone(),
				phase: manifest.entry.phase,
				path: path_text(&path)?,
				sha256: hash(&fs::read(&path)?),
				config_json: serde_json::to_string(&package.config)?,
				dirs,
				status: manifest.status.clone(),
				abi_major: manifest.target.abi_major,
			});
			artifacts.push(Artifact {
				id: manifest.plugin.id.clone(),
				version: manifest.plugin.version.to_string(),
				package_sha256: package.validated.digest.clone(),
			});
		}
		let bytes = wire_bytes(&plan)?;
		let plan_sha256 = hash(&bytes);
		fs::write(runtime.join("foch-host/plan.json"), bytes)?;
		fs::write(
			runtime.join("foch-host/artifacts.json"),
			serde_json::to_vec_pretty(&artifacts)?,
		)?;
		Ok(Prepared {
			plan,
			plan_sha256,
			artifacts,
		})
	}

	pub fn font_cache(&self, data_root: &Path, base_fonts_sha256: &str) -> Option<PathBuf> {
		self.packages
			.iter()
			.find(|package| {
				package
					.validated
					.manifest
					.writes
					.contains(&super::manifest::WriteKind::FontCache)
			})
			.map(|package| {
				let identity =
					hash(format!("{}:{base_fonts_sha256}", package.validated.digest).as_bytes());
				data_root.join("plugins/font-cache").join(identity)
			})
	}
}

/// Ordinary Windows paths for JSON and MAX_PATH consumers, without the
/// verbatim prefix returned by canonicalize. Non-Unicode paths are refused.
pub fn path_text(path: &Path) -> io::Result<String> {
	let text = path
		.to_str()
		.ok_or_else(|| io::Error::other("runtime path is not Unicode"))?;
	if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
		return Ok(format!(r"\\{unc}"));
	}
	Ok(text.strip_prefix(r"\\?\").unwrap_or(text).into())
}

/// Resolve existing ancestors before any write, including nested junctions.
/// Parent traversal is refused rather than creating a missing prefix first.
pub fn ensure_outside_game(game_root: &Path, destination: &Path) -> io::Result<()> {
	if destination
		.components()
		.any(|part| part == std::path::Component::ParentDir)
	{
		return Err(io::Error::other(
			"writable paths cannot contain parent traversal",
		));
	}
	let source = game_root.canonicalize()?;
	let mut existing = destination;
	while !existing.exists() {
		existing = existing
			.parent()
			.filter(|parent| !parent.as_os_str().is_empty())
			.unwrap_or(Path::new("."));
	}
	let existing = existing.canonicalize()?;
	#[cfg(windows)]
	let inside = {
		let source = source.to_string_lossy().replace('/', "\\").to_lowercase();
		let existing = existing.to_string_lossy().replace('/', "\\").to_lowercase();
		existing == source || existing.starts_with(&format!("{}\\", source.trim_end_matches('\\')))
	};
	#[cfg(not(windows))]
	let inside = existing.starts_with(source);
	if inside {
		return Err(io::Error::other(
			"writable paths must be outside the game installation",
		));
	}
	Ok(())
}

pub fn hash(bytes: &[u8]) -> String {
	format!("{:x}", Sha256::digest(bytes))
}

fn write_config(root: &Path, manifest: &Manifest, config: &Value) -> io::Result<()> {
	let mut changes: BTreeMap<String, BTreeMap<(String, String), String>> = BTreeMap::new();
	for (name, field) in &manifest.config {
		let adapter = match field {
			ConfigField::Bool { adapter, .. }
			| ConfigField::Int { adapter, .. }
			| ConfigField::String { adapter, .. } => adapter,
		};
		let Some(adapter) = adapter else {
			continue;
		};
		let ini = store::safe_relative(&adapter.ini).map_err(io::Error::other)?;
		if [&adapter.section, &adapter.key]
			.iter()
			.any(|text| text.is_empty() || text.contains(['\r', '\n', '\0', '[', ']', '=']))
		{
			return Err(io::Error::other("invalid INI adapter section/key"));
		}
		let value = match &config[name] {
			Value::String(value) => value.clone(),
			Value::Bool(value) => if *value { "1" } else { "0" }.into(),
			other => other.to_string(),
		};
		if value.contains(['\r', '\n', '\0']) {
			return Err(io::Error::other("INI value must be a single line"));
		}
		changes
			.entry(ini)
			.or_default()
			.insert((adapter.section.clone(), adapter.key.clone()), value);
	}
	for (relative, changes) in changes {
		let path = root.join(relative);
		let text = match fs::read_to_string(&path) {
			Ok(text) => text,
			Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
			Err(error) => return Err(error),
		};
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(path, update_ini(&text, changes))?;
	}
	Ok(())
}

fn update_ini(text: &str, changes: BTreeMap<(String, String), String>) -> String {
	fn missing(
		out: &mut String,
		section: &str,
		changes: &BTreeMap<(String, String), String>,
		written: &mut BTreeSet<(String, String)>,
	) {
		for ((name, key), value) in changes {
			if name.eq_ignore_ascii_case(section) && written.insert((name.clone(), key.clone())) {
				out.push_str(&format!("{key}={value}\n"));
			}
		}
	}
	let mut out = String::new();
	let mut section = String::new();
	let mut written = BTreeSet::new();
	for line in text.lines() {
		let trimmed = line.trim();
		if let Some(name) = trimmed
			.strip_prefix('[')
			.and_then(|name| name.strip_suffix(']'))
		{
			missing(&mut out, &section, &changes, &mut written);
			section = name.into();
		}
		if let Some((key, _)) = trimmed.split_once('=')
			&& let Some((identity, value)) = changes.iter().find(|((name, candidate), _)| {
				name.eq_ignore_ascii_case(&section) && candidate.eq_ignore_ascii_case(key.trim())
			}) {
			written.insert(identity.clone());
			out.push_str(&format!("{}={value}\n", key.trim()));
		} else {
			out.push_str(line);
			out.push('\n');
		}
	}
	missing(&mut out, &section, &changes, &mut written);
	let mut previous = None;
	for ((section, key), value) in changes {
		if written.contains(&(section.clone(), key.clone())) {
			continue;
		}
		if previous.as_ref() != Some(&section) {
			out.push_str(&format!("\n[{section}]\n"));
			previous = Some(section);
		}
		out.push_str(&format!("{key}={value}\n"));
	}
	out
}

/// Read only complete state records belonging to this frozen run. Missing
/// entries stay not_loaded; a partial final JSONL line is retried next time.
pub fn states(plan: &HostPlan, runtime: &Path) -> io::Result<BTreeMap<String, Value>> {
	plan.validate().map_err(io::Error::other)?;
	if !store::ordinary_metadata(runtime)?.is_dir()
		|| !store::ordinary_metadata(&runtime.join("foch-runtime"))?.is_file()
	{
		return Err(io::Error::other("status requires a prepared runtime layer"));
	}
	let runtime = runtime.canonicalize()?;
	let directory = runtime.join("foch-host");
	if !store::ordinary_metadata(&directory)?.is_dir() {
		return Err(io::Error::other("host directory is not ordinary"));
	}
	let directory = directory.canonicalize()?;
	if directory.parent() != Some(runtime.as_path()) {
		return Err(io::Error::other(
			"host directory resolves outside the runtime",
		));
	}
	let declared = Path::new(&plan.events);
	if !declared
		.file_name()
		.and_then(|name| name.to_str())
		.is_some_and(|name| name.eq_ignore_ascii_case("events.jsonl"))
		|| declared
			.parent()
			.and_then(|parent| parent.canonicalize().ok())
			.as_deref()
			!= Some(directory.as_path())
	{
		return Err(io::Error::other(
			"status events must belong to the prepared runtime",
		));
	}
	let mut states: BTreeMap<_, _> = plan
		.plugins
		.iter()
		.map(|plugin| (plugin.id.clone(), json!({"state":"not_loaded"})))
		.collect();
	let path = directory.join("events.jsonl");
	match store::ordinary_metadata(&path) {
		Ok(metadata) if metadata.is_file() => {}
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(states),
		Err(error) => return Err(error),
		Ok(_) => return Err(io::Error::other("events are not a regular file")),
	}
	let mut options = fs::OpenOptions::new();
	options.read(true);
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		use windows_sys::Win32::Storage::FileSystem::{
			FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
		};
		options
			.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
			.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
	}
	let mut file = match options.open(path) {
		Ok(file) => file,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(states),
		Err(error) => return Err(error),
	};
	if !file.metadata()?.is_file() {
		return Err(io::Error::other("events are not a regular file"));
	}
	#[cfg(windows)]
	{
		use std::os::windows::io::AsRawHandle;
		use windows_sys::Win32::Storage::FileSystem::{
			BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
		};
		let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
		if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
			return Err(io::Error::last_os_error());
		}
		if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks != 1 {
			return Err(io::Error::other("events cannot be linked files"));
		}
	}
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		if file.metadata()?.nlink() != 1 {
			return Err(io::Error::other("events cannot be linked files"));
		}
	}
	let mut text = String::new();
	file.read_to_string(&mut text)?;
	for line in text
		.split_inclusive('\n')
		.filter(|line| line.ends_with('\n'))
	{
		let record: Value = serde_json::from_str(line)?;
		if record["run_id"] == plan.run_id
			&& record["event"] == "state"
			&& let Some(id) = record["plugin_id"].as_str()
			&& states.contains_key(id)
		{
			states.insert(id.into(), record);
		}
	}
	Ok(states)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ini_paths_are_checked_before_any_configuration_write() {
		let root = tempfile::tempdir().unwrap();
		for path in [
			".. /outside.ini",
			"config.ini.",
			"NUL.ini",
			"config.ini:extra",
		] {
			let mut manifest = super::super::builtin::adapters().remove(0);
			let ConfigField::Int {
				adapter: Some(adapter),
				..
			} = manifest.config.get_mut("typo_tolerance").unwrap()
			else {
				panic!("missing adapter")
			};
			adapter.ini = path.into();
			assert!(
				write_config(root.path(), &manifest, &manifest.default_config()).is_err(),
				"{path}"
			);
			assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0, "{path}");
		}
	}

	#[test]
	fn adapter_config_merges_defaults_checks_types_and_preserves_other_ini_keys() {
		let manifest =
			super::super::builtin::adapter("io.github.yozoratempest.eu4-unicode-patch").unwrap();
		assert_eq!(
			effective_config(&manifest, &json!({"typo_tolerance":2})).unwrap(),
			json!({"typo_tolerance":2,"fuzzy_pinyin":0})
		);
		for bad in [
			json!({"typo_tolerance":3}),
			json!({"fuzzy_pinyin":true}),
			json!({"unknown":1}),
		] {
			assert!(effective_config(&manifest, &bad).is_err());
		}
		let text = update_ini(
			"[search]\nother=yes\ntypo_tolerance=0\n",
			BTreeMap::from([(("search".into(), "typo_tolerance".into()), "2".into())]),
		);
		assert!(text.contains("other=yes\n"));
		assert!(text.contains("typo_tolerance=2\n"));
		assert!(!text.contains("typo_tolerance=0"));
	}

	#[test]
	fn missing_real_artifacts_are_refused_even_when_an_adapter_exists() {
		let root = tempfile::tempdir().unwrap();
		let adapter = super::super::builtin::adapters().remove(0);
		let game = GameIdentity {
			game: "eu4".into(),
			version: "1.37.5".parse().unwrap(),
			platform: planner::WINDOWS_X64.into(),
		};
		let chosen = Selection {
			id: adapter.plugin.id,
			version: adapter.plugin.version,
			enabled: true,
			config: json!({}),
		};
		assert!(resolve(&game, root.path(), &[chosen]).is_err());
	}

	#[test]
	fn missing_ini_keys_are_added_to_the_existing_section() {
		let text = update_ini(
			"[search]\ntypo_tolerance=0\n[other]\nkeep=yes\n",
			BTreeMap::from([
				(("search".into(), "typo_tolerance".into()), "2".into()),
				(("search".into(), "fuzzy_pinyin".into()), "1".into()),
			]),
		);
		assert_eq!(text.matches("[search]").count(), 1);
		assert!(text.find("fuzzy_pinyin=1").unwrap() < text.find("[other]").unwrap());
	}

	#[test]
	fn configuration_defaults_and_bounds_are_validated() {
		let mut manifest = super::super::builtin::adapters().remove(0);
		manifest.config.insert(
			"bad".into(),
			ConfigField::Int {
				default: 9,
				min: Some(0),
				max: Some(2),
				description: None,
				adapter: None,
			},
		);
		assert!(effective_config(&manifest, &json!({})).is_err());
		manifest.config.insert(
			"bad".into(),
			ConfigField::Int {
				default: 1,
				min: Some(2),
				max: Some(0),
				description: None,
				adapter: None,
			},
		);
		assert!(effective_config(&manifest, &json!({"bad":1})).is_err());
	}

	#[test]
	fn oversized_configuration_cannot_produce_an_unreadable_host_plan() {
		let mut plan = HostPlan {
			format: 1,
			run_id: "size".into(),
			game_version: "1.37.5".into(),
			events: r"C:\rt\events.jsonl".into(),
			plugins: vec![HostPlugin {
				id: "sample".into(),
				version: "1.0.0".into(),
				kind: Kind::Native,
				phase: Phase::Entry,
				path: r"C:\rt\sample.dll".into(),
				sha256: "a".repeat(64),
				config_json: "{}".into(),
				dirs: BTreeMap::new(),
				status: None,
				abi_major: Some(1),
			}],
		};
		assert!(wire_bytes(&plan).is_ok());
		plan.plugins[0].config_json = json!({"text":"x".repeat(MAX_PLAN_BYTES)}).to_string();
		assert!(wire_bytes(&plan).is_err());
	}
}
