//! Central registration of the built-in EU4 DLL plugins.
//!
//! One row per plugin, in the shape of a Homebrew formula and a Unity Mod
//! Manager `Info.json`: name, author, homepage, version, and the release URL.
//! A row writes `depends_on` or `conflicts_with` only when it has one. `plan`
//! reads those lists. Install parses the release URL for `gh release download`.

use super::builtin;
use super::store;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// One plugin this row refuses to load beside, and why.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
	pub id: String,
	pub because: String,
}

/// One registered plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
	pub id: String,
	pub name: String,
	pub author: String,
	pub homepage: String,
	pub version: String,
	pub url: String,
	/// `owner/name`, taken from the release URL. The homepage names the same repository.
	pub repo: String,
	/// GitHub release tag parsed from `url`. This can differ from `version`.
	pub tag: String,
	/// Release asset file name parsed from `url`.
	pub asset: String,
	pub depends_on: Vec<String>,
	pub conflicts_with: Vec<Conflict>,
	/// Package-relative paths dropped before the adapter is bound.
	pub omit: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
	schema: u32,
	plugin: Vec<Row>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
	id: String,
	name: String,
	author: String,
	homepage: String,
	version: String,
	url: String,
	#[serde(default)]
	depends_on: Vec<String>,
	#[serde(default)]
	conflicts_with: Vec<Conflict>,
	#[serde(default)]
	omit: Vec<String>,
}

struct ReleaseUrl {
	repo: String,
	tag: String,
	asset: String,
}

const SCHEMA: u32 = 1;

/// The plugins compiled into this build, in registry order.
pub fn plugins() -> &'static [Plugin] {
	static TABLE: OnceLock<Vec<Plugin>> = OnceLock::new();
	TABLE.get_or_init(|| load(include_str!("registry.toml")).expect("plugin registry is valid"))
}

pub(crate) fn load(text: &str) -> Result<Vec<Plugin>, String> {
	let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
	if file.schema != SCHEMA {
		return Err(format!("schema {} is not {SCHEMA}", file.schema));
	}
	let mut plugins = Vec::with_capacity(file.plugin.len());
	let mut ids = BTreeSet::new();
	let mut repos = BTreeSet::new();
	for row in file.plugin {
		if row.id.is_empty() || !ids.insert(row.id.clone()) {
			return Err(format!("duplicate or empty plugin id {}", row.hint()));
		}
		if row.name.is_empty()
			|| row.author.is_empty()
			|| row.version.is_empty()
			|| row.homepage.is_empty()
			|| row.url.is_empty()
		{
			return Err(format!(
				"{} is missing its name, author, version, homepage, or url",
				row.id
			));
		}
		let release = release_from_url(&row.url)?;
		let homepage_repo = repo_from_homepage(&row.homepage)?;
		if homepage_repo != release.repo {
			return Err(format!(
				"{} homepage {homepage_repo} is not the release repository {}",
				row.id, release.repo
			));
		}
		if !repos.insert(release.repo.clone()) {
			return Err(format!("{} reuses repository {}", row.id, release.repo));
		}
		let conflict_ids: Vec<String> = row
			.conflicts_with
			.iter()
			.map(|conflict| conflict.id.clone())
			.collect();
		for conflict in &row.conflicts_with {
			if conflict.id.is_empty() || conflict.because.trim().is_empty() {
				return Err(format!(
					"{} has a conflicts_with entry without an id and a because",
					row.id
				));
			}
		}
		if lists_itself(&row.id, &row.depends_on) || lists_itself(&row.id, &conflict_ids) {
			return Err(format!("{} names itself", row.id));
		}
		if has_duplicate(&row.depends_on) || has_duplicate(&conflict_ids) {
			return Err(format!("{} repeats a dependency or a conflict", row.id));
		}
		for path in &row.omit {
			store::safe_relative(path)
				.map_err(|error| format!("{} omit {path}: {error}", row.id))?;
		}
		plugins.push(Plugin {
			id: row.id,
			name: row.name,
			author: row.author,
			homepage: row.homepage,
			version: row.version,
			url: row.url,
			repo: release.repo,
			tag: release.tag,
			asset: release.asset,
			depends_on: row.depends_on,
			conflicts_with: row.conflicts_with,
			omit: row.omit,
		});
	}
	let registered: BTreeSet<_> = plugins.iter().map(|plugin| plugin.id.as_str()).collect();
	for plugin in &plugins {
		for other in plugin
			.depends_on
			.iter()
			.chain(plugin.conflicts_with.iter().map(|conflict| &conflict.id))
		{
			if !registered.contains(other.as_str()) {
				return Err(format!(
					"{} names {other}, which is not registered",
					plugin.id
				));
			}
		}
		if plugin.depends_on.iter().any(|dependency| {
			plugin
				.conflicts_with
				.iter()
				.any(|conflict| conflict.id == *dependency)
		}) {
			return Err(format!(
				"{} depends on a plugin it conflicts with",
				plugin.id
			));
		}
	}
	let adapters = builtin::adapters();
	let adapter_ids: BTreeSet<_> = adapters
		.iter()
		.map(|manifest| manifest.plugin.id.as_str())
		.collect();
	if registered != adapter_ids {
		return Err(format!(
			"registered ids {registered:?} are not the built-in adapters {adapter_ids:?}"
		));
	}
	for plugin in &plugins {
		let manifest = adapters
			.iter()
			.find(|manifest| manifest.plugin.id == plugin.id)
			.expect("id set matches");
		if manifest.plugin.name != plugin.name {
			return Err(format!(
				"{} name {:?} does not match the adapter {:?}",
				plugin.id, plugin.name, manifest.plugin.name
			));
		}
		if manifest.plugin.version.to_string() != plugin.version {
			return Err(format!(
				"{} version {} does not match the adapter {}",
				plugin.id, plugin.version, manifest.plugin.version
			));
		}
		if manifest.plugin.authors.len() != 1 || manifest.plugin.authors[0] != plugin.author {
			return Err(format!(
				"{} author {:?} does not match the adapter {:?}",
				plugin.id, plugin.author, manifest.plugin.authors
			));
		}
	}
	Ok(plugins)
}

fn release_from_url(url: &str) -> Result<ReleaseUrl, String> {
	let Some(path) = url.strip_prefix("https://github.com/") else {
		return Err(format!("{url} is not a GitHub release download URL"));
	};
	let mut parts = path.split('/');
	let Some(owner) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{url} is missing the owner"));
	};
	let Some(name) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{url} is missing the repository name"));
	};
	if parts.next() != Some("releases") || parts.next() != Some("download") {
		return Err(format!("{url} is not a /releases/download/ URL"));
	}
	let Some(tag) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{url} is missing the tag"));
	};
	let Some(asset) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{url} is missing the asset"));
	};
	if parts.next().is_some() {
		return Err(format!("{url} has a path after the asset"));
	}
	if tag.contains(['?', '#']) || asset.contains(['?', '#']) {
		return Err(format!("{url} must not carry a query or a fragment"));
	}
	Ok(ReleaseUrl {
		repo: format!("{owner}/{name}"),
		tag: tag.to_string(),
		asset: asset.to_string(),
	})
}

fn repo_from_homepage(homepage: &str) -> Result<String, String> {
	let trimmed = homepage.trim().trim_end_matches('/');
	let Some(path) = trimmed.strip_prefix("https://github.com/") else {
		return Err(format!(
			"{homepage} is not a https://github.com/owner/name URL"
		));
	};
	let mut parts = path.split('/');
	let Some(owner) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{homepage} is missing the owner"));
	};
	let Some(name) = parts.next().filter(|part| !part.is_empty()) else {
		return Err(format!("{homepage} is missing the repository name"));
	};
	if parts.next().is_some() {
		return Err(format!("{homepage} has a path after the repository name"));
	}
	Ok(format!("{owner}/{name}"))
}

fn lists_itself(id: &str, others: &[String]) -> bool {
	others.iter().any(|other| other == id)
}

fn has_duplicate(values: &[String]) -> bool {
	let mut seen = BTreeSet::new();
	values.iter().any(|value| !seen.insert(value.as_str()))
}

trait RowHint {
	fn hint(&self) -> &str;
}

impl RowHint for Row {
	fn hint(&self) -> &str {
		if self.id.is_empty() {
			&self.url
		} else {
			&self.id
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn each_builtin_plugin_is_one_row() {
		let plugins = plugins();
		assert_eq!(plugins.len(), builtin::adapters().len());
		let unicode = plugins
			.iter()
			.find(|plugin| plugin.id == "io.github.yozoratempest.eu4-unicode-patch")
			.unwrap();
		assert_eq!(unicode.name, "EU4 Unicode Patch");
		assert_eq!(unicode.author, "YozoraTempest");
		assert_eq!(unicode.version, "0.1.15");
		assert_eq!(
			unicode.homepage,
			"https://github.com/YozoraTempest/EU4-Unicode-Patch"
		);
		assert_eq!(unicode.repo, "YozoraTempest/EU4-Unicode-Patch");
		assert_eq!(unicode.tag, "v0.1.15");
		assert_eq!(unicode.asset, "EU4UnicodePatch-1.37.5-v0.1.15.zip");
		assert!(unicode.depends_on.is_empty());
		assert!(unicode.omit.is_empty());
		assert_eq!(
			unicode.conflicts_with,
			[Conflict {
				id: "io.github.matanki-saito.eu4dll".into(),
				because:
					"EU4 Unicode Patch v0.1.15 refuses to initialize when plugin64.dll is loaded."
						.into(),
			}]
		);
		let menu = plugins
			.iter()
			.find(|plugin| plugin.id == "io.github.yozoratempest.eu4-menu-patch")
			.unwrap();
		assert!(menu.depends_on.is_empty());
		assert!(menu.conflicts_with.is_empty());
		assert!(menu.omit.is_empty());
		let eu4dll = plugins
			.iter()
			.find(|plugin| plugin.id == "io.github.matanki-saito.eu4dll")
			.unwrap();
		assert_eq!(eu4dll.version, "93.0.0");
		assert_eq!(eu4dll.tag, "93");
		assert_eq!(eu4dll.asset, "steam_EU4_fixdll_build_93.zip");
		assert!(eu4dll.depends_on.is_empty());
		assert!(eu4dll.conflicts_with.is_empty());
		assert_eq!(eu4dll.omit.len(), 4);
	}

	#[test]
	fn homepage_must_name_the_release_repository() {
		let text = include_str!("registry.toml").replace(
			"homepage = \"https://github.com/YozoraTempest/EU4-Unicode-Patch\"",
			"homepage = \"https://github.com/YozoraTempest/somewhere-else\"",
		);
		let error = load(&text).unwrap_err();
		assert!(error.contains("homepage"), "{error}");
	}

	#[test]
	fn release_url_must_be_a_github_download() {
		let text = include_str!("registry.toml").replace(
			"https://github.com/YozoraTempest/EU4-Unicode-Patch/releases/download/v0.1.15/EU4UnicodePatch-1.37.5-v0.1.15.zip",
			"https://github.com/YozoraTempest/EU4-Unicode-Patch/archive/refs/tags/v0.1.15.zip",
		);
		let error = load(&text).unwrap_err();
		assert!(error.contains("releases/download"), "{error}");
	}
}
