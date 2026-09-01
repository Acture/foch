use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use keyvalues_parser::{Obj, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use steamlocate::SteamDir;

/// A canonical decimal representation of a Steam 64-bit identifier.
///
/// Keeping the representation as a string prevents accidental loss of
/// precision when the value is serialized through JSON consumers.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SteamId(String);

impl SteamId {
	pub fn new(value: u64) -> Self {
		Self(value.to_string())
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}

	pub fn as_u64(&self) -> u64 {
		self.0
			.parse()
			.expect("SteamId construction guarantees a canonical u64")
	}
}

impl fmt::Display for SteamId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

impl From<u64> for SteamId {
	fn from(value: u64) -> Self {
		Self::new(value)
	}
}

impl FromStr for SteamId {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let parsed = value
			.parse::<u64>()
			.map_err(|err| format!("invalid Steam 64-bit id {value:?}: {err}"))?;
		if parsed.to_string() != value {
			return Err(format!(
				"Steam 64-bit id {value:?} is not canonical decimal"
			));
		}
		Ok(Self(value.to_string()))
	}
}

impl Serialize for SteamId {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for SteamId {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		value.parse().map_err(serde::de::Error::custom)
	}
}

/// Steam uses `-1` when an installed Workshop item has no usable manifest id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "id", rename_all = "snake_case")]
pub enum WorkshopManifestId {
	Unavailable,
	Id(SteamId),
}

impl WorkshopManifestId {
	pub fn id(&self) -> Option<&SteamId> {
		match self {
			Self::Unavailable => None,
			Self::Id(id) => Some(id),
		}
	}
}

/// Stable identity of one installed Workshop version as reported by Steam's
/// `appworkshop_<appid>.acf`.
///
/// Deliberately excludes local paths: paths locate an installation, while the
/// ACF tuple identifies the version whose content may be cached.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopInstallIdentity {
	pub app_id: u32,
	pub workshop_id: SteamId,
	pub manifest_id: SteamId,
}

/// One record from `WorkshopItemsInstalled`, paired with its same-library
/// content path. The source ACF and content tree are never opened for writing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledWorkshopItem {
	pub app_id: u32,
	pub workshop_id: SteamId,
	pub manifest: WorkshopManifestId,
	pub size_bytes: u64,
	pub time_updated: u64,
	pub ugc_handle: Option<SteamId>,
	pub content_path: PathBuf,
	pub manifest_path: PathBuf,
}

impl InstalledWorkshopItem {
	pub fn require_manifest_id(&self) -> Result<&SteamId, SteamWorkshopError> {
		self.manifest
			.id()
			.ok_or_else(|| SteamWorkshopError::UnavailableManifest {
				workshop_id: self.workshop_id.clone(),
				manifest_path: self.manifest_path.clone(),
			})
	}

	/// Return the path-independent identity Steam assigned to this install.
	///
	/// Steam's `manifest = -1` sentinel cannot identify a content version and
	/// is therefore rejected rather than converted into a cacheable identity.
	pub fn identity(&self) -> Result<WorkshopInstallIdentity, SteamWorkshopError> {
		Ok(WorkshopInstallIdentity {
			app_id: self.app_id,
			workshop_id: self.workshop_id.clone(),
			manifest_id: self.require_manifest_id()?.clone(),
		})
	}
}

/// A Workshop content root and the `appworkshop_<appid>.acf` from the same
/// Steam library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopLibrary {
	pub app_id: u32,
	pub content_root: PathBuf,
	pub manifest_path: PathBuf,
	items: BTreeMap<SteamId, InstalledWorkshopItem>,
}

impl WorkshopLibrary {
	pub fn from_paths(
		app_id: u32,
		content_root: impl Into<PathBuf>,
		manifest_path: impl Into<PathBuf>,
	) -> Result<Self, SteamWorkshopError> {
		let content_root = content_root.into();
		let manifest_path = manifest_path.into();
		if !content_root.is_dir() {
			return Err(SteamWorkshopError::MissingContentRoot { content_root });
		}
		if !manifest_path.is_file() {
			return Err(SteamWorkshopError::MissingWorkshopManifest {
				content_root,
				manifest_path,
			});
		}
		let (content_root, manifest_path) =
			validate_workshop_library_pair(app_id, &content_root, &manifest_path)?;

		let items = read_workshop_manifest(app_id, &content_root, &manifest_path)?;
		Ok(Self {
			app_id,
			content_root,
			manifest_path,
			items,
		})
	}

	pub fn item(&self, workshop_id: &SteamId) -> Option<&InstalledWorkshopItem> {
		self.items.get(workshop_id)
	}

	pub fn items(&self) -> impl Iterator<Item = &InstalledWorkshopItem> {
		self.items.values()
	}
}

/// Read-only installed Workshop catalog across all Steam libraries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopCatalog {
	pub app_id: u32,
	libraries: Vec<WorkshopLibrary>,
}

impl SteamWorkshopCatalog {
	pub fn discover(app_id: u32) -> Result<Self, SteamWorkshopError> {
		let steam = SteamDir::locate().map_err(|err| SteamWorkshopError::SteamDiscovery {
			detail: err.to_string(),
		})?;
		Self::discover_from_steam_root(steam.path(), app_id)
	}

	pub fn discover_from_steam_root(
		steam_root: &Path,
		app_id: u32,
	) -> Result<Self, SteamWorkshopError> {
		let steam =
			SteamDir::from_dir(steam_root).map_err(|err| SteamWorkshopError::SteamDiscovery {
				detail: format!("invalid Steam root {}: {err}", steam_root.display()),
			})?;
		let mut library_roots =
			steam
				.library_paths()
				.map_err(|err| SteamWorkshopError::SteamDiscovery {
					detail: format!(
						"failed to inspect Steam libraries under {}: {err}",
						steam_root.display()
					),
				})?;
		library_roots.push(steam_root.to_path_buf());
		let mut seen = HashSet::new();
		library_roots.retain(|path| seen.insert(normalize_candidate(path)));

		let mut libraries = Vec::new();
		for library_root in library_roots {
			let workshop_root = library_root.join("steamapps").join("workshop");
			let content_root = workshop_root.join("content").join(app_id.to_string());
			if !content_root.exists() {
				continue;
			}
			let manifest_path = workshop_root.join(format!("appworkshop_{app_id}.acf"));
			libraries.push(WorkshopLibrary::from_paths(
				app_id,
				content_root,
				manifest_path,
			)?);
		}

		if libraries.is_empty() {
			return Err(SteamWorkshopError::NoWorkshopLibraries {
				steam_root: steam_root.to_path_buf(),
				app_id,
			});
		}
		Ok(Self { app_id, libraries })
	}

	/// Build a catalog from one explicit content-root/ACF pair. This is the
	/// disambiguation path when the same item exists in multiple libraries.
	pub fn from_override(
		app_id: u32,
		content_root: impl Into<PathBuf>,
		manifest_path: impl Into<PathBuf>,
	) -> Result<Self, SteamWorkshopError> {
		Self::from_library_paths(app_id, [(content_root.into(), manifest_path.into())])
	}

	/// Re-read an exact set of content-root/ACF pairs. Callers can retain these
	/// paths and use this constructor before and after a product run without
	/// rediscovering Steam libraries in between.
	pub fn from_library_paths(
		app_id: u32,
		pairs: impl IntoIterator<Item = (PathBuf, PathBuf)>,
	) -> Result<Self, SteamWorkshopError> {
		let mut libraries = Vec::new();
		let mut content_roots = HashSet::new();
		let mut manifest_paths = HashSet::new();
		for (content_root, manifest_path) in pairs {
			let library = WorkshopLibrary::from_paths(app_id, content_root, manifest_path)?;
			if !content_roots.insert(library.content_root.clone())
				|| !manifest_paths.insert(library.manifest_path.clone())
			{
				return Err(SteamWorkshopError::DuplicateLibraryPair {
					content_root: library.content_root,
					manifest_path: library.manifest_path,
				});
			}
			libraries.push(library);
		}
		if libraries.is_empty() {
			return Err(SteamWorkshopError::EmptyLibraryPairs { app_id });
		}
		Ok(Self { app_id, libraries })
	}

	pub fn libraries(&self) -> &[WorkshopLibrary] {
		&self.libraries
	}

	/// Look up an ACF record without requiring a usable manifest id or present
	/// content directory. A missing record returns `Ok(None)`; `manifest = -1`
	/// remains visible on the returned item.
	pub fn item(
		&self,
		workshop_id: &SteamId,
	) -> Result<Option<&InstalledWorkshopItem>, SteamWorkshopError> {
		let matches = self
			.libraries
			.iter()
			.filter_map(|library| library.item(workshop_id))
			.collect::<Vec<_>>();
		match matches.as_slice() {
			[] => Ok(None),
			[item] => Ok(Some(item)),
			_ => Err(SteamWorkshopError::AmbiguousItem {
				app_id: self.app_id,
				workshop_id: workshop_id.clone(),
				manifest_paths: matches
					.iter()
					.map(|item| item.manifest_path.clone())
					.collect(),
			}),
		}
	}

	/// Resolve an installed item for product use. Unlike [`Self::item`], this
	/// rejects missing ACF records, Steam's `-1` manifest sentinel, and missing
	/// content directories.
	pub fn require_item(
		&self,
		workshop_id: &SteamId,
	) -> Result<&InstalledWorkshopItem, SteamWorkshopError> {
		let item = self
			.item(workshop_id)?
			.ok_or_else(|| SteamWorkshopError::MissingItem {
				app_id: self.app_id,
				workshop_id: workshop_id.clone(),
			})?;
		item.require_manifest_id()?;
		validate_workshop_item_root(item)?;
		Ok(item)
	}
}

#[derive(Debug)]
pub enum SteamWorkshopError {
	SteamDiscovery {
		detail: String,
	},
	NoWorkshopLibraries {
		steam_root: PathBuf,
		app_id: u32,
	},
	MissingContentRoot {
		content_root: PathBuf,
	},
	MissingWorkshopManifest {
		content_root: PathBuf,
		manifest_path: PathBuf,
	},
	EmptyLibraryPairs {
		app_id: u32,
	},
	DuplicateLibraryPair {
		content_root: PathBuf,
		manifest_path: PathBuf,
	},
	InvalidWorkshopLibraryPair {
		content_root: PathBuf,
		manifest_path: PathBuf,
		detail: String,
	},
	SymlinkWorkshopPath {
		path: PathBuf,
	},
	ReadManifest {
		manifest_path: PathBuf,
		source: std::io::Error,
	},
	InvalidManifest {
		manifest_path: PathBuf,
		detail: String,
	},
	AppIdMismatch {
		manifest_path: PathBuf,
		expected: u32,
		actual: u32,
	},
	MissingItem {
		app_id: u32,
		workshop_id: SteamId,
	},
	UnavailableManifest {
		workshop_id: SteamId,
		manifest_path: PathBuf,
	},
	MissingItemContent {
		workshop_id: SteamId,
		content_path: PathBuf,
	},
	EscapedWorkshopItemContent {
		workshop_id: SteamId,
		content_root: PathBuf,
		content_path: PathBuf,
	},
	AmbiguousItem {
		app_id: u32,
		workshop_id: SteamId,
		manifest_paths: Vec<PathBuf>,
	},
}

impl fmt::Display for SteamWorkshopError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::SteamDiscovery { detail } => formatter.write_str(detail),
			Self::NoWorkshopLibraries { steam_root, app_id } => write!(
				formatter,
				"no Workshop content root for Steam app {app_id} under {}",
				steam_root.display()
			),
			Self::MissingContentRoot { content_root } => write!(
				formatter,
				"Workshop content root is missing or not a directory: {}",
				content_root.display()
			),
			Self::MissingWorkshopManifest {
				content_root,
				manifest_path,
			} => write!(
				formatter,
				"Workshop content root {} has no same-library manifest {}",
				content_root.display(),
				manifest_path.display()
			),
			Self::EmptyLibraryPairs { app_id } => {
				write!(
					formatter,
					"no Workshop library pairs supplied for app {app_id}"
				)
			}
			Self::DuplicateLibraryPair {
				content_root,
				manifest_path,
			} => write!(
				formatter,
				"duplicate Workshop library path pair: content={} manifest={}",
				content_root.display(),
				manifest_path.display()
			),
			Self::InvalidWorkshopLibraryPair {
				content_root,
				manifest_path,
				detail,
			} => write!(
				formatter,
				"invalid Workshop library pair: content={} manifest={}: {detail}",
				content_root.display(),
				manifest_path.display()
			),
			Self::SymlinkWorkshopPath { path } => write!(
				formatter,
				"Workshop library paths must not contain symlink components: {}",
				path.display()
			),
			Self::ReadManifest {
				manifest_path,
				source,
			} => write!(
				formatter,
				"failed to read Workshop manifest {}: {source}",
				manifest_path.display()
			),
			Self::InvalidManifest {
				manifest_path,
				detail,
			} => write!(
				formatter,
				"invalid Workshop manifest {}: {detail}",
				manifest_path.display()
			),
			Self::AppIdMismatch {
				manifest_path,
				expected,
				actual,
			} => write!(
				formatter,
				"Workshop manifest {} has appid {actual}, expected {expected}",
				manifest_path.display()
			),
			Self::MissingItem {
				app_id,
				workshop_id,
			} => write!(
				formatter,
				"Workshop item {workshop_id} is absent from appworkshop_{app_id}.acf"
			),
			Self::UnavailableManifest {
				workshop_id,
				manifest_path,
			} => write!(
				formatter,
				"Workshop item {workshop_id} has manifest = -1 in {}",
				manifest_path.display()
			),
			Self::MissingItemContent {
				workshop_id,
				content_path,
			} => write!(
				formatter,
				"Workshop item {workshop_id} has no content directory {}",
				content_path.display()
			),
			Self::EscapedWorkshopItemContent {
				workshop_id,
				content_root,
				content_path,
			} => write!(
				formatter,
				"Workshop item {workshop_id} resolves outside its paired content root {}: {}",
				content_root.display(),
				content_path.display()
			),
			Self::AmbiguousItem {
				app_id,
				workshop_id,
				manifest_paths,
			} => write!(
				formatter,
				"Workshop item {workshop_id} for app {app_id} is present in multiple libraries: {}",
				manifest_paths
					.iter()
					.map(|path| path.display().to_string())
					.collect::<Vec<_>>()
					.join(", ")
			),
		}
	}
}

impl std::error::Error for SteamWorkshopError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::ReadManifest { source, .. } => Some(source),
			_ => None,
		}
	}
}

fn validate_workshop_item_root(item: &InstalledWorkshopItem) -> Result<(), SteamWorkshopError> {
	let content_root = item
		.content_path
		.parent()
		.expect("Workshop item paths are constructed below a content root");
	let metadata = fs::symlink_metadata(&item.content_path).map_err(|_| {
		SteamWorkshopError::MissingItemContent {
			workshop_id: item.workshop_id.clone(),
			content_path: item.content_path.clone(),
		}
	})?;
	if metadata.file_type().is_symlink() {
		return Err(SteamWorkshopError::SymlinkWorkshopPath {
			path: item.content_path.clone(),
		});
	}
	if !metadata.file_type().is_dir() {
		return Err(SteamWorkshopError::MissingItemContent {
			workshop_id: item.workshop_id.clone(),
			content_path: item.content_path.clone(),
		});
	}
	let canonical_item = fs::canonicalize(&item.content_path).map_err(|_| {
		SteamWorkshopError::MissingItemContent {
			workshop_id: item.workshop_id.clone(),
			content_path: item.content_path.clone(),
		}
	})?;
	if canonical_item != item.content_path || canonical_item.parent() != Some(content_root) {
		return Err(SteamWorkshopError::EscapedWorkshopItemContent {
			workshop_id: item.workshop_id.clone(),
			content_root: content_root.to_path_buf(),
			content_path: canonical_item,
		});
	}
	// The stored path is byte-for-byte the canonical direct child validated
	// above, so callers receive a canonical item root without changing the API.
	Ok(())
}

fn validate_workshop_library_pair(
	app_id: u32,
	content_root: &Path,
	manifest_path: &Path,
) -> Result<(PathBuf, PathBuf), SteamWorkshopError> {
	reject_symlink_tail(content_root, 5)?;
	reject_symlink_tail(manifest_path, 4)?;

	let canonical_content = fs::canonicalize(content_root).map_err(|error| {
		invalid_library_pair(
			content_root,
			manifest_path,
			format!("failed to canonicalize content root: {error}"),
		)
	})?;
	let canonical_manifest = fs::canonicalize(manifest_path).map_err(|error| {
		invalid_library_pair(
			content_root,
			manifest_path,
			format!("failed to canonicalize Workshop ACF: {error}"),
		)
	})?;

	let expected_app_id = app_id.to_string();
	if canonical_content.file_name().and_then(|name| name.to_str())
		!= Some(expected_app_id.as_str())
	{
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			format!("content root must end in content/{app_id}"),
		));
	}
	let content_dir = canonical_content.parent().ok_or_else(|| {
		invalid_library_pair(
			content_root,
			manifest_path,
			"content root has no content parent".to_string(),
		)
	})?;
	if content_dir.file_name().and_then(|name| name.to_str()) != Some("content") {
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			format!("content root must end in content/{app_id}"),
		));
	}
	let workshop_root = content_dir.parent().ok_or_else(|| {
		invalid_library_pair(
			content_root,
			manifest_path,
			"content root has no workshop parent".to_string(),
		)
	})?;
	if workshop_root.file_name().and_then(|name| name.to_str()) != Some("workshop") {
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			"content root must belong to <library>/steamapps/workshop".to_string(),
		));
	}
	let steamapps_root = workshop_root.parent().ok_or_else(|| {
		invalid_library_pair(
			content_root,
			manifest_path,
			"Workshop root has no steamapps parent".to_string(),
		)
	})?;
	if steamapps_root.file_name().and_then(|name| name.to_str()) != Some("steamapps") {
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			"content root must belong to <library>/steamapps/workshop".to_string(),
		));
	}
	if steamapps_root.parent().is_none() {
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			"Workshop root has no Steam library parent".to_string(),
		));
	}

	let expected_manifest_name = format!("appworkshop_{app_id}.acf");
	if canonical_manifest
		.file_name()
		.and_then(|name| name.to_str())
		!= Some(expected_manifest_name.as_str())
		|| canonical_manifest.parent() != Some(workshop_root)
	{
		return Err(invalid_library_pair(
			content_root,
			manifest_path,
			format!("Workshop ACF must be the same-library {expected_manifest_name}"),
		));
	}

	// Recheck the caller-supplied path components after canonicalization so a
	// symlink cannot be introduced between the first check and identity binding.
	reject_symlink_tail(content_root, 5)?;
	reject_symlink_tail(manifest_path, 4)?;

	Ok((canonical_content, canonical_manifest))
}

fn reject_symlink_tail(path: &Path, components: usize) -> Result<(), SteamWorkshopError> {
	let mut current = Some(path);
	for _ in 0..components {
		let candidate = current.ok_or_else(|| SteamWorkshopError::InvalidWorkshopLibraryPair {
			content_root: path.to_path_buf(),
			manifest_path: path.to_path_buf(),
			detail: "Workshop path is too shallow".to_string(),
		})?;
		let metadata = fs::symlink_metadata(candidate).map_err(|error| {
			SteamWorkshopError::InvalidWorkshopLibraryPair {
				content_root: path.to_path_buf(),
				manifest_path: path.to_path_buf(),
				detail: format!(
					"failed to inspect Workshop path component {}: {error}",
					candidate.display()
				),
			}
		})?;
		if metadata.file_type().is_symlink() {
			return Err(SteamWorkshopError::SymlinkWorkshopPath {
				path: candidate.to_path_buf(),
			});
		}
		current = candidate.parent();
	}
	Ok(())
}

fn invalid_library_pair(
	content_root: &Path,
	manifest_path: &Path,
	detail: String,
) -> SteamWorkshopError {
	SteamWorkshopError::InvalidWorkshopLibraryPair {
		content_root: content_root.to_path_buf(),
		manifest_path: manifest_path.to_path_buf(),
		detail,
	}
}

fn read_workshop_manifest(
	app_id: u32,
	content_root: &Path,
	manifest_path: &Path,
) -> Result<BTreeMap<SteamId, InstalledWorkshopItem>, SteamWorkshopError> {
	let text = std::fs::read_to_string(manifest_path).map_err(|source| {
		SteamWorkshopError::ReadManifest {
			manifest_path: manifest_path.to_path_buf(),
			source,
		}
	})?;
	let parsed = keyvalues_parser::parse(&text)
		.map_err(|err| invalid_manifest(manifest_path, format!("KeyValues parse failed: {err}")))?;
	if !parsed.bases.is_empty() {
		return Err(invalid_manifest(
			manifest_path,
			"#base directives are not supported".to_string(),
		));
	}
	if parsed.key.as_ref() != "AppWorkshop" {
		return Err(invalid_manifest(
			manifest_path,
			format!("expected root key AppWorkshop, found {:?}", parsed.key),
		));
	}
	let root = parsed.value.get_obj().ok_or_else(|| {
		invalid_manifest(manifest_path, "AppWorkshop must be an object".to_string())
	})?;
	reject_duplicate_keys(root, "AppWorkshop", manifest_path)?;

	let actual_app_id = parse_u32_field(
		&required_string(root, "appid", "AppWorkshop", manifest_path)?,
		"AppWorkshop.appid",
		manifest_path,
	)?;
	if actual_app_id != app_id {
		return Err(SteamWorkshopError::AppIdMismatch {
			manifest_path: manifest_path.to_path_buf(),
			expected: app_id,
			actual: actual_app_id,
		});
	}
	let installed = required_object(root, "WorkshopItemsInstalled", "AppWorkshop", manifest_path)?;

	let mut items = BTreeMap::new();
	for (raw_workshop_id, values) in installed.iter() {
		let workshop_id = raw_workshop_id.parse::<SteamId>().map_err(|err| {
			invalid_manifest(
				manifest_path,
				format!("invalid WorkshopItemsInstalled key: {err}"),
			)
		})?;
		if workshop_id.as_u64() == 0 {
			return Err(invalid_manifest(
				manifest_path,
				"WorkshopItemsInstalled contains item id 0".to_string(),
			));
		}
		let value = values.first().ok_or_else(|| {
			invalid_manifest(
				manifest_path,
				format!("WorkshopItemsInstalled.{workshop_id} has no value"),
			)
		})?;
		let item_object = value.get_obj().ok_or_else(|| {
			invalid_manifest(
				manifest_path,
				format!("WorkshopItemsInstalled.{workshop_id} must be an object"),
			)
		})?;
		let context = format!("AppWorkshop.WorkshopItemsInstalled.{workshop_id}");
		let size_bytes = parse_u64_field(
			&required_string(item_object, "size", &context, manifest_path)?,
			&format!("{context}.size"),
			manifest_path,
		)?;
		let time_updated = parse_u64_field(
			&required_string(item_object, "timeupdated", &context, manifest_path)?,
			&format!("{context}.timeupdated"),
			manifest_path,
		)?;
		let raw_manifest = required_string(item_object, "manifest", &context, manifest_path)?;
		let manifest = if raw_manifest == "-1" {
			WorkshopManifestId::Unavailable
		} else {
			let id = raw_manifest.parse::<SteamId>().map_err(|err| {
				invalid_manifest(manifest_path, format!("{context}.manifest: {err}"))
			})?;
			if id.as_u64() == 0 {
				return Err(invalid_manifest(
					manifest_path,
					format!("{context}.manifest must not be 0"),
				));
			}
			WorkshopManifestId::Id(id)
		};
		let ugc_handle = optional_string(item_object, "ugchandle", &context, manifest_path)?
			.map(|value| {
				value.parse::<SteamId>().map_err(|err| {
					invalid_manifest(manifest_path, format!("{context}.ugchandle: {err}"))
				})
			})
			.transpose()?;
		let content_path = content_root.join(workshop_id.as_str());
		let item = InstalledWorkshopItem {
			app_id,
			workshop_id: workshop_id.clone(),
			manifest,
			size_bytes,
			time_updated,
			ugc_handle,
			content_path,
			manifest_path: manifest_path.to_path_buf(),
		};
		if items.insert(workshop_id.clone(), item).is_some() {
			return Err(invalid_manifest(
				manifest_path,
				format!("duplicate Workshop item id {workshop_id}"),
			));
		}
	}
	Ok(items)
}

fn invalid_manifest(manifest_path: &Path, detail: String) -> SteamWorkshopError {
	SteamWorkshopError::InvalidManifest {
		manifest_path: manifest_path.to_path_buf(),
		detail,
	}
}

fn reject_duplicate_keys(
	object: &Obj<'_>,
	context: &str,
	manifest_path: &Path,
) -> Result<(), SteamWorkshopError> {
	for (key, values) in object.iter() {
		if values.len() != 1 {
			return Err(invalid_manifest(
				manifest_path,
				format!("duplicate key {context}.{key}"),
			));
		}
		if let Some(child) = values[0].get_obj() {
			reject_duplicate_keys(child, &format!("{context}.{key}"), manifest_path)?;
		}
	}
	Ok(())
}

fn required_value<'object, 'text>(
	object: &'object Obj<'text>,
	key: &str,
	context: &str,
	manifest_path: &Path,
) -> Result<&'object Value<'text>, SteamWorkshopError> {
	let values = object
		.get(key)
		.ok_or_else(|| invalid_manifest(manifest_path, format!("missing key {context}.{key}")))?;
	match values.as_slice() {
		[value] => Ok(value),
		[] => Err(invalid_manifest(
			manifest_path,
			format!("key {context}.{key} has no value"),
		)),
		_ => Err(invalid_manifest(
			manifest_path,
			format!("duplicate key {context}.{key}"),
		)),
	}
}

fn required_string(
	object: &Obj<'_>,
	key: &str,
	context: &str,
	manifest_path: &Path,
) -> Result<String, SteamWorkshopError> {
	required_value(object, key, context, manifest_path)?
		.get_str()
		.map(ToOwned::to_owned)
		.ok_or_else(|| {
			invalid_manifest(
				manifest_path,
				format!("key {context}.{key} must be a string"),
			)
		})
}

fn optional_string(
	object: &Obj<'_>,
	key: &str,
	context: &str,
	manifest_path: &Path,
) -> Result<Option<String>, SteamWorkshopError> {
	let Some(values) = object.get(key) else {
		return Ok(None);
	};
	match values.as_slice() {
		[value] => value
			.get_str()
			.map(|value| Some(value.to_string()))
			.ok_or_else(|| {
				invalid_manifest(
					manifest_path,
					format!("key {context}.{key} must be a string"),
				)
			}),
		[] => Err(invalid_manifest(
			manifest_path,
			format!("key {context}.{key} has no value"),
		)),
		_ => Err(invalid_manifest(
			manifest_path,
			format!("duplicate key {context}.{key}"),
		)),
	}
}

fn required_object<'object, 'text>(
	object: &'object Obj<'text>,
	key: &str,
	context: &str,
	manifest_path: &Path,
) -> Result<&'object Obj<'text>, SteamWorkshopError> {
	match required_value(object, key, context, manifest_path)? {
		Value::Obj(value) => Ok(value),
		Value::Str(_) => Err(invalid_manifest(
			manifest_path,
			format!("key {context}.{key} must be an object"),
		)),
	}
}

fn parse_u64_field(
	value: &str,
	field: &str,
	manifest_path: &Path,
) -> Result<u64, SteamWorkshopError> {
	value.parse::<u64>().map_err(|err| {
		invalid_manifest(
			manifest_path,
			format!("{field} must be an unsigned decimal integer: {err}"),
		)
	})
}

fn parse_u32_field(
	value: &str,
	field: &str,
	manifest_path: &Path,
) -> Result<u32, SteamWorkshopError> {
	value.parse::<u32>().map_err(|err| {
		invalid_manifest(
			manifest_path,
			format!("{field} must be an unsigned decimal integer: {err}"),
		)
	})
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatedSteamApp {
	pub app_id: u32,
	pub steam_root: PathBuf,
	pub library_root: PathBuf,
	pub game_root: PathBuf,
	pub build_id: Option<u64>,
}

pub fn find_steam_root_path() -> Option<PathBuf> {
	SteamDir::locate()
		.ok()
		.map(|steam| steam.path().to_path_buf())
}

pub fn locate_steam_app(app_id: u32) -> Result<LocatedSteamApp, String> {
	let steam = SteamDir::locate().map_err(|err| format!("failed to locate Steam: {err}"))?;
	locate_steam_app_from(&steam, app_id)
}

pub fn locate_steam_app_from_root(
	steam_root: &Path,
	app_id: u32,
) -> Result<LocatedSteamApp, String> {
	let steam = SteamDir::from_dir(steam_root)
		.map_err(|err| format!("invalid Steam root {}: {err}", steam_root.display()))?;
	locate_steam_app_from(&steam, app_id)
}

fn locate_steam_app_from(steam: &SteamDir, app_id: u32) -> Result<LocatedSteamApp, String> {
	let (app, library) = steam
		.find_app(app_id)
		.map_err(|err| format!("failed to inspect Steam app {app_id}: {err}"))?
		.ok_or_else(|| format!("Steam app {app_id} is not installed"))?;
	let game_root = library.resolve_app_dir(&app);
	if !game_root.is_dir() {
		return Err(format!(
			"Steam app {app_id} manifest points to missing directory {}",
			game_root.display()
		));
	}
	Ok(LocatedSteamApp {
		app_id,
		steam_root: steam.path().to_path_buf(),
		library_root: library.path().to_path_buf(),
		game_root,
		build_id: app.build_id,
	})
}

pub fn steam_library_paths(steam_root: &Path) -> Vec<PathBuf> {
	let mut paths = SteamDir::from_dir(steam_root)
		.and_then(|steam| steam.library_paths())
		.unwrap_or_default();
	paths.push(steam_root.to_path_buf());

	let mut seen = HashSet::new();
	paths.retain(|path| seen.insert(normalize_candidate(path)));
	paths
}

pub fn steam_workshop_mod_path(steam_root: &Path, app_id: u32, steam_id: &str) -> Option<PathBuf> {
	steam_library_paths(steam_root)
		.into_iter()
		.map(|library| {
			library
				.join("steamapps")
				.join("workshop")
				.join("content")
				.join(app_id.to_string())
				.join(steam_id)
		})
		.find(|candidate| candidate.is_dir())
}

pub fn steam_game_install_path(steam_root: &Path, app_id: u32) -> Option<PathBuf> {
	locate_steam_app_from_root(steam_root, app_id)
		.ok()
		.map(|app| app.game_root)
}

fn normalize_candidate(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::{
		SteamId, SteamWorkshopCatalog, SteamWorkshopError, WorkshopInstallIdentity,
		WorkshopManifestId, locate_steam_app_from_root, steam_game_install_path,
		steam_library_paths, steam_workshop_mod_path,
	};
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;

	fn vdf_path_value(path: &Path) -> String {
		path.to_string_lossy().replace('\\', "\\\\")
	}

	fn write_steam_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
		let tmp = TempDir::new().expect("temp dir");
		let steam_root = tmp.path().join("Steam");
		let lib2 = tmp.path().join("SteamLibrary2");
		std::fs::create_dir_all(steam_root.join("steamapps")).expect("create steamapps");
		std::fs::create_dir_all(lib2.join("steamapps").join("common")).expect("create common");
		std::fs::write(
			steam_root.join("steamapps").join("libraryfolders.vdf"),
			format!(
				r#""libraryfolders"
{{
	"0" {{ "path" "{}" }}
	"1" {{ "path" "{}" }}
}}"#,
				vdf_path_value(&steam_root),
				vdf_path_value(&lib2)
			),
		)
		.expect("write vdf");
		std::fs::write(
			lib2.join("steamapps").join("appmanifest_236850.acf"),
			r#""AppState"
{
	"appid" "236850"
	"installdir" "Europa Universalis IV"
	"buildid" "123456"
}"#,
		)
		.expect("write manifest");
		let game_dir = lib2
			.join("steamapps")
			.join("common")
			.join("Europa Universalis IV");
		std::fs::create_dir_all(&game_dir).expect("create game dir");
		(tmp, steam_root, game_dir)
	}

	fn write_workshop_fixture(
		library_root: &Path,
		app_id: u32,
		installed_items: &str,
	) -> (PathBuf, PathBuf) {
		let workshop_root = library_root.join("steamapps").join("workshop");
		let content_root = workshop_root.join("content").join(app_id.to_string());
		let manifest_path = workshop_root.join(format!("appworkshop_{app_id}.acf"));
		std::fs::create_dir_all(&content_root).expect("create Workshop content root");
		std::fs::write(
			&manifest_path,
			format!(
				r#""AppWorkshop"
{{
	"appid" "{app_id}"
	"WorkshopItemsInstalled"
	{{
{installed_items}
	}}
}}"#
			),
		)
		.expect("write Workshop manifest");
		(content_root, manifest_path)
	}

	fn installed_item(workshop_id: &str, manifest_id: &str) -> String {
		format!(
			r#"		"{workshop_id}"
		{{
			"size" "12345"
			"timeupdated" "1780000000"
			"manifest" "{manifest_id}"
			"ugchandle" "18446744073709551615"
		}}"#
		)
	}

	#[test]
	fn library_paths_include_alternate_library() {
		let (tmp, steam_root, _) = write_steam_fixture();
		let paths = steam_library_paths(&steam_root);
		assert!(paths.iter().any(|item| item == &steam_root));
		assert!(
			paths
				.iter()
				.any(|item| item == &tmp.path().join("SteamLibrary2"))
		);
	}

	#[test]
	fn located_app_carries_build_and_library_identity() {
		let (tmp, steam_root, game_dir) = write_steam_fixture();
		let app = locate_steam_app_from_root(&steam_root, 236850).expect("locate app");
		assert_eq!(app.game_root, game_dir);
		assert_eq!(app.library_root, tmp.path().join("SteamLibrary2"));
		assert_eq!(app.build_id, Some(123456));
		assert_eq!(
			steam_game_install_path(&steam_root, 236850).as_deref(),
			Some(app.game_root.as_path())
		);
	}

	#[test]
	fn workshop_item_searches_all_libraries() {
		let (tmp, steam_root, _) = write_steam_fixture();
		let workshop_item = tmp
			.path()
			.join("SteamLibrary2/steamapps/workshop/content/236850/42");
		std::fs::create_dir_all(&workshop_item).expect("create workshop item");
		assert_eq!(
			steam_workshop_mod_path(&steam_root, 236850, "42").as_deref(),
			Some(workshop_item.as_path())
		);
	}

	#[test]
	fn steam_ids_are_canonical_decimal_json_strings() {
		let id = "9007199254740993".parse::<SteamId>().expect("valid id");
		assert_eq!(id.as_u64(), 9_007_199_254_740_993);
		assert_eq!(serde_json::to_string(&id).unwrap(), r#""9007199254740993""#);
		assert_eq!(
			serde_json::from_str::<SteamId>(r#""9007199254740993""#).unwrap(),
			id
		);
		assert!("09007199254740993".parse::<SteamId>().is_err());
		assert!(serde_json::from_str::<SteamId>("9007199254740993").is_err());
	}

	#[test]
	fn workshop_identity_is_path_independent_and_serde_stable() {
		let identity = WorkshopInstallIdentity {
			app_id: 236_850,
			workshop_id: "42".parse().unwrap(),
			manifest_id: "9007199254740993".parse().unwrap(),
		};
		let encoded = serde_json::to_string(&identity).unwrap();
		assert_eq!(
			encoded,
			r#"{"app_id":236850,"workshop_id":"42","manifest_id":"9007199254740993"}"#
		);
		assert_eq!(
			serde_json::from_str::<WorkshopInstallIdentity>(&encoded).unwrap(),
			identity
		);
		assert!(!encoded.contains("path"));
	}

	#[test]
	fn strict_catalog_distinguishes_missing_unavailable_and_missing_content() {
		let tmp = TempDir::new().expect("temp dir");
		let items = format!(
			"{}\n{}\n{}",
			installed_item("42", "9007199254740993"),
			installed_item("43", "-1"),
			installed_item("44", "123")
		);
		let (content_root, manifest_path) = write_workshop_fixture(tmp.path(), 236850, &items);
		std::fs::create_dir_all(content_root.join("42")).expect("create item 42");
		std::fs::create_dir_all(content_root.join("43")).expect("create item 43");

		let catalog =
			SteamWorkshopCatalog::from_override(236850, &content_root, &manifest_path).unwrap();
		let item_42 = catalog
			.require_item(&"42".parse().unwrap())
			.expect("strict valid item");
		assert_eq!(
			item_42.require_manifest_id().unwrap().as_str(),
			"9007199254740993"
		);
		assert_eq!(
			item_42.identity().unwrap(),
			WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: "42".parse().unwrap(),
				manifest_id: "9007199254740993".parse().unwrap(),
			}
		);
		let item_43 = catalog.item(&"43".parse().unwrap()).unwrap().unwrap();
		assert_eq!(item_43.manifest, WorkshopManifestId::Unavailable);
		assert!(matches!(
			item_43.identity(),
			Err(SteamWorkshopError::UnavailableManifest { .. })
		));
		assert!(matches!(
			catalog.require_item(&"43".parse().unwrap()),
			Err(SteamWorkshopError::UnavailableManifest { .. })
		));
		assert!(matches!(
			catalog.require_item(&"44".parse().unwrap()),
			Err(SteamWorkshopError::MissingItemContent { .. })
		));
		assert_eq!(catalog.item(&"45".parse().unwrap()).unwrap(), None);
		assert!(matches!(
			catalog.require_item(&"45".parse().unwrap()),
			Err(SteamWorkshopError::MissingItem { .. })
		));
	}

	#[test]
	fn manifest_requires_matching_app_and_unique_keys() {
		let tmp = TempDir::new().expect("temp dir");
		let duplicate_item = r#"		"42"
		{
			"size" "1"
			"timeupdated" "2"
			"manifest" "3"
			"manifest" "4"
		}"#;
		let (content_root, manifest_path) =
			write_workshop_fixture(tmp.path(), 236850, duplicate_item);
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &content_root, &manifest_path),
			Err(SteamWorkshopError::InvalidManifest { .. })
		));

		let valid_item = installed_item("42", "3");
		let (_, manifest_path) = write_workshop_fixture(tmp.path(), 236850, &valid_item);
		let text = std::fs::read_to_string(&manifest_path).unwrap().replacen(
			"\"236850\"",
			"\"111111\"",
			1,
		);
		std::fs::write(&manifest_path, text).unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &content_root, &manifest_path),
			Err(SteamWorkshopError::AppIdMismatch {
				expected: 236850,
				actual: 111111,
				..
			})
		));
	}

	#[test]
	fn content_root_requires_its_paired_manifest() {
		let tmp = TempDir::new().expect("temp dir");
		let content_root = tmp.path().join("content/236850");
		std::fs::create_dir_all(&content_root).unwrap();
		let missing_manifest = tmp.path().join("appworkshop_236850.acf");
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, content_root, missing_manifest),
			Err(SteamWorkshopError::MissingWorkshopManifest { .. })
		));
	}

	#[test]
	fn explicit_pair_rejects_cross_library_and_wrong_app_paths() {
		let tmp = TempDir::new().expect("temp dir");
		let entry = installed_item("42", "3");
		let (first_content, first_manifest) =
			write_workshop_fixture(&tmp.path().join("LibraryA"), 236850, &entry);
		let (_second_content, second_manifest) =
			write_workshop_fixture(&tmp.path().join("LibraryB"), 236850, &entry);
		std::fs::create_dir_all(first_content.join("42")).unwrap();

		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &first_content, &second_manifest),
			Err(SteamWorkshopError::InvalidWorkshopLibraryPair { .. })
		));

		let wrong_content = first_content
			.parent()
			.expect("content parent")
			.join("111111");
		std::fs::create_dir_all(&wrong_content).unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &wrong_content, &first_manifest),
			Err(SteamWorkshopError::InvalidWorkshopLibraryPair { .. })
		));

		let wrong_manifest = first_manifest.with_file_name("appworkshop_111111.acf");
		std::fs::copy(&first_manifest, &wrong_manifest).unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &first_content, &wrong_manifest),
			Err(SteamWorkshopError::InvalidWorkshopLibraryPair { .. })
		));
	}

	#[cfg(unix)]
	#[test]
	fn explicit_pair_rejects_symlink_roots_and_components() {
		use std::os::unix::fs::symlink;

		let tmp = TempDir::new().expect("temp dir");
		let entry = installed_item("42", "3");
		let real_library = tmp.path().join("RealLibrary");
		let (real_content, real_manifest) = write_workshop_fixture(&real_library, 236850, &entry);
		std::fs::create_dir_all(real_content.join("42")).unwrap();

		let linked_library = tmp.path().join("LinkedLibrary");
		symlink(&real_library, &linked_library).unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(
				236850,
				linked_library.join("steamapps/workshop/content/236850"),
				linked_library.join("steamapps/workshop/appworkshop_236850.acf"),
			),
			Err(SteamWorkshopError::SymlinkWorkshopPath { .. })
		));

		let component_library = tmp.path().join("ComponentLibrary");
		std::fs::create_dir_all(&component_library).unwrap();
		symlink(
			real_library.join("steamapps"),
			component_library.join("steamapps"),
		)
		.unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(
				236850,
				component_library.join("steamapps/workshop/content/236850"),
				component_library.join("steamapps/workshop/appworkshop_236850.acf"),
			),
			Err(SteamWorkshopError::SymlinkWorkshopPath { .. })
		));

		let linked_acf = real_manifest.with_file_name("appworkshop_236850.link.acf");
		symlink(&real_manifest, &linked_acf).unwrap();
		assert!(matches!(
			SteamWorkshopCatalog::from_override(236850, &real_content, &linked_acf),
			Err(SteamWorkshopError::SymlinkWorkshopPath { .. })
		));
	}

	#[cfg(unix)]
	#[test]
	fn require_item_rejects_symlink_item_root() {
		use std::os::unix::fs::symlink;

		let tmp = TempDir::new().expect("temp dir");
		let (content_root, manifest_path) =
			write_workshop_fixture(tmp.path(), 236850, &installed_item("42", "3"));
		let external = tmp.path().join("outside-item");
		std::fs::create_dir_all(&external).unwrap();
		symlink(&external, content_root.join("42")).unwrap();

		let catalog =
			SteamWorkshopCatalog::from_override(236850, content_root, manifest_path).unwrap();
		assert!(matches!(
			catalog.require_item(&"42".parse().unwrap()),
			Err(SteamWorkshopError::SymlinkWorkshopPath { .. })
		));
	}

	#[test]
	fn duplicate_items_across_libraries_are_ambiguous_until_overridden() {
		let (tmp, steam_root, _) = write_steam_fixture();
		let lib2 = tmp.path().join("SteamLibrary2");
		let entry = installed_item("42", "9007199254740993");
		let (root_content, root_manifest) = write_workshop_fixture(&steam_root, 236850, &entry);
		let (lib2_content, lib2_manifest) = write_workshop_fixture(&lib2, 236850, &entry);
		std::fs::create_dir_all(root_content.join("42")).unwrap();
		std::fs::create_dir_all(lib2_content.join("42")).unwrap();

		let catalog = SteamWorkshopCatalog::discover_from_steam_root(&steam_root, 236850).unwrap();
		assert!(matches!(
			catalog.require_item(&"42".parse().unwrap()),
			Err(SteamWorkshopError::AmbiguousItem { .. })
		));

		let catalog =
			SteamWorkshopCatalog::from_override(236850, &lib2_content, &lib2_manifest).unwrap();
		assert_eq!(
			catalog
				.require_item(&"42".parse().unwrap())
				.unwrap()
				.content_path,
			std::fs::canonicalize(&lib2_content).unwrap().join("42")
		);

		assert!(matches!(
			SteamWorkshopCatalog::from_library_paths(236850, Vec::new()),
			Err(SteamWorkshopError::EmptyLibraryPairs { .. })
		));
		assert!(matches!(
			SteamWorkshopCatalog::from_library_paths(
				236850,
				vec![
					(root_content.clone(), root_manifest.clone()),
					(root_content, root_manifest),
				],
			),
			Err(SteamWorkshopError::DuplicateLibraryPair { .. })
		));
	}

	#[cfg(unix)]
	#[test]
	fn catalog_only_needs_read_access() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = TempDir::new().expect("temp dir");
		let (content_root, manifest_path) =
			write_workshop_fixture(tmp.path(), 236850, &installed_item("42", "3"));
		let item_path = content_root.join("42");
		std::fs::create_dir_all(&item_path).unwrap();
		std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o444)).unwrap();
		std::fs::set_permissions(&content_root, std::fs::Permissions::from_mode(0o555)).unwrap();
		std::fs::set_permissions(&item_path, std::fs::Permissions::from_mode(0o555)).unwrap();

		let catalog =
			SteamWorkshopCatalog::from_override(236850, &content_root, &manifest_path).unwrap();
		assert!(catalog.require_item(&"42".parse().unwrap()).is_ok());

		std::fs::set_permissions(&content_root, std::fs::Permissions::from_mode(0o755)).unwrap();
		std::fs::set_permissions(&item_path, std::fs::Permissions::from_mode(0o755)).unwrap();
	}
}
