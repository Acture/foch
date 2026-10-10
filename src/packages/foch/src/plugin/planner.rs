//! Turning a playset selection into one launch's frozen load order.
//!
//! [`plan`] is pure: given the game identity, the manifests available in the
//! store, and the user's selection, it resolves versions, checks
//! compatibility, orders plugins by their declared dependencies and ordering
//! constraints, and reports every problem it finds. It performs no I/O and
//! never loads a DLL. The loader executes exactly what it returns; it does not
//! re-order anything itself.

use super::manifest::{Kind, Manifest, Phase};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};

/// The ABI major versions this Foch build's host can load.
pub const SUPPORTED_ABI_MAJOR: &[u32] = &[1];

/// The platform tuple of the only target this build supports.
pub const WINDOWS_X64: &str = "windows-x86_64";

/// The game a plan targets.
#[derive(Clone, Debug)]
pub struct GameIdentity {
	pub game: String,
	pub version: Version,
	pub platform: String,
}

/// Normalize an EU4 launcher version string (for example `v1.37.5.0`) to a
/// semver `Version`: drop a leading `v` and keep the first three numeric
/// components, filling missing ones with zero.
pub fn parse_game_version(raw: &str) -> Option<Version> {
	let trimmed = raw.trim().trim_start_matches(['v', 'V']);
	let mut parts = trimmed.split('.').map(|part| part.trim());
	let mut numbers = [0u64; 3];
	for slot in &mut numbers {
		match parts.next() {
			Some(part) => *slot = part.parse().ok()?,
			None => break,
		}
	}
	Some(Version::new(numbers[0], numbers[1], numbers[2]))
}

/// One plugin the user selected for a playset.
#[derive(Clone, Debug)]
pub struct Selection {
	pub id: String,
	/// The exact version the playset pinned.
	pub version: Version,
	pub enabled: bool,
	/// Configuration values, already validated against the manifest schema.
	pub config: serde_json::Value,
}

/// A resolved plugin in load order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
	pub id: String,
	pub version: Version,
	pub kind: Kind,
	pub phase: Phase,
}

/// A problem that keeps a plan from being launchable, or a warning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Diagnostic {
	/// The selected version is not in the store.
	Missing { id: String, version: Version },
	/// Platform or game version is out of the declared range.
	Incompatible { id: String, reason: String },
	/// A native plugin's ABI major is not supported by this host.
	UnsupportedAbi { id: String, abi_major: u32 },
	/// A required dependency is absent, disabled, or version-mismatched.
	Dependency {
		id: String,
		requires: String,
		reason: String,
	},
	/// Two enabled plugins declare a mutual conflict.
	Conflict {
		id: String,
		with: String,
		reason: Option<String>,
	},
	/// Ordering constraints form a cycle.
	Cycle { ids: Vec<String> },
	/// Two plugins ship a file at the same install-relative path with
	/// different contents; the process would load only one.
	FileCollision { path: String, ids: Vec<String> },
}

/// The outcome of planning: the order to load, plus every diagnostic. The plan
/// is launchable only when `errors` is empty.
#[derive(Clone, Debug, Default)]
pub struct Resolution {
	pub order: Vec<Resolved>,
	pub errors: Vec<Diagnostic>,
	pub warnings: Vec<Diagnostic>,
}

impl Resolution {
	pub fn is_launchable(&self) -> bool {
		self.errors.is_empty()
	}
}

/// Resolve and order the enabled selection against the store.
pub fn plan(
	game: &GameIdentity,
	store: &BTreeMap<String, BTreeMap<Version, Manifest>>,
	selection: &[Selection],
) -> Resolution {
	let mut resolution = Resolution::default();

	// Resolve each enabled selection to its manifest, collecting hard errors.
	let mut resolved: BTreeMap<String, &Manifest> = BTreeMap::new();
	for item in selection.iter().filter(|item| item.enabled) {
		let Some(versions) = store.get(&item.id) else {
			resolution.errors.push(Diagnostic::Missing {
				id: item.id.clone(),
				version: item.version.clone(),
			});
			continue;
		};
		let Some(manifest) = versions.get(&item.version) else {
			resolution.errors.push(Diagnostic::Missing {
				id: item.id.clone(),
				version: item.version.clone(),
			});
			continue;
		};
		if let Some(reason) = incompatibility(game, manifest) {
			resolution.errors.push(Diagnostic::Incompatible {
				id: item.id.clone(),
				reason,
			});
			continue;
		}
		if manifest.entry.kind == Kind::Native
			&& let Some(abi) = manifest.target.abi_major
			&& !SUPPORTED_ABI_MAJOR.contains(&abi)
		{
			resolution.errors.push(Diagnostic::UnsupportedAbi {
				id: item.id.clone(),
				abi_major: abi,
			});
			continue;
		}
		resolved.insert(item.id.clone(), manifest);
	}

	check_dependencies(&resolved, &mut resolution);
	check_conflicts(&resolved, &mut resolution);
	check_file_collisions(&resolved, &mut resolution);

	// Drop plugins whose dependencies failed before ordering, so a dependent
	// of a missing dependency is never placed in the order.
	let usable: BTreeMap<String, &Manifest> = resolved
		.iter()
		.filter(|(id, _)| !has_blocking_error(&resolution, id))
		.map(|(id, manifest)| (id.clone(), *manifest))
		.collect();

	match order(&usable) {
		Ok(order) => {
			for pair in order.windows(2) {
				if pair[0].phase > pair[1].phase {
					resolution.errors.push(Diagnostic::Incompatible {
						id: pair[1].id.clone(),
						reason: "ordering requires a later startup phase before an earlier one"
							.into(),
					});
				}
			}
			resolution.order = order;
		}
		Err(cycle) => resolution.errors.push(Diagnostic::Cycle { ids: cycle }),
	}
	resolution
}

fn incompatibility(game: &GameIdentity, manifest: &Manifest) -> Option<String> {
	if manifest.target.game != game.game {
		return Some(format!(
			"declares game {}, not {}",
			manifest.target.game, game.game
		));
	}
	if manifest.target.platform != game.platform {
		return Some(format!(
			"declares platform {}, not {}",
			manifest.target.platform, game.platform
		));
	}
	if !manifest.target.game_versions.matches(&game.version) {
		return Some(format!(
			"declares game versions {}, which excludes {}",
			manifest.target.game_versions, game.version
		));
	}
	None
}

fn check_dependencies(resolved: &BTreeMap<String, &Manifest>, resolution: &mut Resolution) {
	for (id, manifest) in resolved {
		for dependency in &manifest.depends {
			let requires = format!("{} {}", dependency.id, dependency.version);
			match resolved.get(&dependency.id) {
				None => resolution.errors.push(Diagnostic::Dependency {
					id: id.clone(),
					requires,
					reason: "not enabled or not resolvable".into(),
				}),
				Some(dependency_manifest) => {
					if !dependency
						.version
						.matches(&dependency_manifest.plugin.version)
					{
						resolution.errors.push(Diagnostic::Dependency {
							id: id.clone(),
							requires,
							reason: format!(
								"resolved {} does not satisfy the requirement",
								dependency_manifest.plugin.version
							),
						});
					}
				}
			}
		}
	}
}

fn check_conflicts(resolved: &BTreeMap<String, &Manifest>, resolution: &mut Resolution) {
	let mut reported = BTreeSet::new();
	for (id, manifest) in resolved {
		for conflict in &manifest.conflicts {
			if let Some(other) = resolved.get(&conflict.id)
				&& conflict.version.matches(&other.plugin.version)
			{
				let pair = if id < &conflict.id {
					(id, &conflict.id)
				} else {
					(&conflict.id, id)
				};
				if reported.insert(pair) {
					resolution.errors.push(Diagnostic::Conflict {
						id: id.clone(),
						with: conflict.id.clone(),
						reason: conflict.reason.clone(),
					});
				}
			}
		}
	}
}

fn check_file_collisions(resolved: &BTreeMap<String, &Manifest>, resolution: &mut Resolution) {
	// Windows reuses already loaded modules by basename. Entry DLLs must be
	// distinct even with identical bytes: their init/configuration is per id.
	let mut entry_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
	for (id, manifest) in resolved {
		entry_names
			.entry(
				manifest
					.entry
					.path
					.rsplit(['/', '\\'])
					.next()
					.unwrap_or_default()
					.to_lowercase(),
			)
			.or_default()
			.push(id.clone());
	}
	for (path, ids) in entry_names {
		if ids.len() > 1 {
			resolution
				.errors
				.push(Diagnostic::FileCollision { path, ids });
		}
	}
	// Dependency DLL basename -> digest -> plugin ids declaring it.
	let mut by_path: BTreeMap<String, BTreeMap<&str, BTreeSet<&str>>> = BTreeMap::new();
	for (id, manifest) in resolved {
		for file in &manifest.files {
			if !file.path.to_ascii_lowercase().ends_with(".dll") {
				continue;
			}
			let name = file
				.path
				.rsplit(['/', '\\'])
				.next()
				.unwrap()
				.to_ascii_lowercase();
			by_path
				.entry(name)
				.or_default()
				.entry(file.sha256.as_str())
				.or_default()
				.insert(id.as_str());
		}
	}
	for (path, digests) in by_path {
		if digests.len() > 1 {
			let mut ids: BTreeSet<&str> = BTreeSet::new();
			for owners in digests.values() {
				ids.extend(owners.iter().copied());
			}
			resolution.errors.push(Diagnostic::FileCollision {
				path: path.to_string(),
				ids: ids.into_iter().map(str::to_string).collect(),
			});
		}
	}
}

fn has_blocking_error(resolution: &Resolution, id: &str) -> bool {
	resolution.errors.iter().any(|diagnostic| match diagnostic {
		Diagnostic::Dependency { id: who, .. } => who == id,
		Diagnostic::Conflict { id: a, with: b, .. } => a == id || b == id,
		Diagnostic::FileCollision { ids, .. } => ids.iter().any(|who| who == id),
		_ => false,
	})
}

/// Topologically order the usable plugins. Edges: a hard dependency and a
/// `load_after` both make this plugin come later; `load_before` is the mirror.
/// Unicode is pinned first among ties by sorting the ready set by id, which
/// also makes the output deterministic. Returns the ids in a cycle on failure.
fn order(usable: &BTreeMap<String, &Manifest>) -> Result<Vec<Resolved>, Vec<String>> {
	// Build "must come after" edges only among plugins that are present.
	let mut after: BTreeMap<&str, BTreeSet<&str>> = usable
		.keys()
		.map(|id| (id.as_str(), BTreeSet::new()))
		.collect();
	for (id, manifest) in usable {
		let here = id.as_str();
		for dependency in &manifest.depends {
			if usable.contains_key(&dependency.id) {
				after.get_mut(here).unwrap().insert(dependency.id.as_str());
			}
		}
		for other in &manifest.load_after {
			if usable.contains_key(other) {
				after.get_mut(here).unwrap().insert(other.as_str());
			}
		}
		for other in &manifest.load_before {
			if let Some(edges) = after.get_mut(other.as_str()) {
				edges.insert(here);
			}
		}
	}

	// Kahn's algorithm, picking the smallest ready id for a stable order.
	let mut remaining: BTreeMap<&str, usize> =
		after.iter().map(|(id, deps)| (*id, deps.len())).collect();
	let mut order = Vec::new();
	while !remaining.is_empty() {
		let Some(next) = remaining
			.iter()
			.filter(|&(_, &count)| count == 0)
			.map(|(id, _)| *id)
			.min_by_key(|id| (usable[*id].entry.phase, *id))
		else {
			let mut cycle: Vec<String> = remaining.keys().map(|id| id.to_string()).collect();
			cycle.sort();
			return Err(cycle);
		};
		remaining.remove(next);
		for (other, deps) in &after {
			if deps.contains(next) && remaining.contains_key(other) {
				*remaining.get_mut(other).unwrap() -= 1;
			}
		}
		let manifest = usable[next];
		order.push(Resolved {
			id: next.to_string(),
			version: manifest.plugin.version.clone(),
			kind: manifest.entry.kind.clone(),
			phase: manifest.entry.phase,
		});
	}
	Ok(order)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::plugin::manifest::Manifest;

	fn manifest(id: &str, version: &str, body: &str) -> Manifest {
		// `body` is placed at the root, before any table header, so bare keys
		// (load_after/load_before) and `[[depends]]`/`[[conflicts]]`/`[[files]]`
		// arrays all attach to the root table rather than `[entry]`.
		let text = format!(
			r#"
schema = 1
{body}
[plugin]
id = "{id}"
name = "{id}"
version = "{version}"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "=1.37.5"
abi_major = 1
[entry]
kind = "native"
path = "{id}.dll"
phase = "deferred"
"#
		);
		Manifest::parse(&text).unwrap()
	}

	fn store(manifests: Vec<Manifest>) -> BTreeMap<String, BTreeMap<Version, Manifest>> {
		let mut store: BTreeMap<String, BTreeMap<Version, Manifest>> = BTreeMap::new();
		for manifest in manifests {
			store
				.entry(manifest.plugin.id.clone())
				.or_default()
				.insert(manifest.plugin.version.clone(), manifest);
		}
		store
	}

	fn game() -> GameIdentity {
		GameIdentity {
			game: "eu4".into(),
			version: Version::parse("1.37.5").unwrap(),
			platform: "windows-x86_64".into(),
		}
	}

	fn select(id: &str, version: &str) -> Selection {
		Selection {
			id: id.into(),
			version: Version::parse(version).unwrap(),
			enabled: true,
			config: serde_json::Value::Null,
		}
	}

	#[test]
	fn orders_dependencies_before_dependents() {
		let store = store(vec![
			manifest("base", "1.0.0", ""),
			manifest(
				"leaf",
				"1.0.0",
				"[[depends]]\nid = \"base\"\nversion = \"^1\"",
			),
		]);
		let resolution = plan(
			&game(),
			&store,
			&[select("leaf", "1.0.0"), select("base", "1.0.0")],
		);
		assert!(resolution.is_launchable());
		let ids: Vec<&str> = resolution.order.iter().map(|r| r.id.as_str()).collect();
		assert_eq!(ids, ["base", "leaf"]);
	}

	#[test]
	fn missing_dependency_drops_the_dependent() {
		let store = store(vec![manifest(
			"leaf",
			"1.0.0",
			"[[depends]]\nid = \"base\"\nversion = \"*\"",
		)]);
		let resolution = plan(&game(), &store, &[select("leaf", "1.0.0")]);
		assert!(!resolution.is_launchable());
		assert!(resolution.order.is_empty());
		assert!(matches!(
			resolution.errors[0],
			Diagnostic::Dependency { .. }
		));
	}

	#[test]
	fn dependency_version_must_match() {
		let store = store(vec![
			manifest("base", "1.0.0", ""),
			manifest(
				"leaf",
				"1.0.0",
				"[[depends]]\nid = \"base\"\nversion = \"^2\"",
			),
		]);
		let resolution = plan(
			&game(),
			&store,
			&[select("base", "1.0.0"), select("leaf", "1.0.0")],
		);
		assert!(!resolution.is_launchable());
	}

	#[test]
	fn declared_conflicts_are_reported_once() {
		let store = store(vec![
			manifest(
				"a",
				"1.0.0",
				"[[conflicts]]\nid = \"b\"\nversion = \"*\"\nreason = \"same hook\"",
			),
			manifest("b", "1.0.0", "[[conflicts]]\nid = \"a\"\nversion = \"*\""),
		]);
		let resolution = plan(
			&game(),
			&store,
			&[select("a", "1.0.0"), select("b", "1.0.0")],
		);
		let conflicts = resolution
			.errors
			.iter()
			.filter(|d| matches!(d, Diagnostic::Conflict { .. }))
			.count();
		assert_eq!(conflicts, 1);
	}

	#[test]
	fn cycles_are_detected() {
		let store = store(vec![
			manifest("a", "1.0.0", "load_after = [\"b\"]"),
			manifest("b", "1.0.0", "load_after = [\"a\"]"),
		]);
		let resolution = plan(
			&game(),
			&store,
			&[select("a", "1.0.0"), select("b", "1.0.0")],
		);
		assert!(matches!(resolution.errors[0], Diagnostic::Cycle { .. }));
	}

	#[test]
	fn incompatible_game_version_is_rejected() {
		let store = store(vec![manifest("a", "1.0.0", "")]);
		let mut game = game();
		game.version = Version::parse("1.38.0").unwrap();
		let resolution = plan(&game, &store, &[select("a", "1.0.0")]);
		assert!(matches!(
			resolution.errors[0],
			Diagnostic::Incompatible { .. }
		));
	}

	#[test]
	fn disabled_selections_are_ignored() {
		let store = store(vec![manifest("a", "1.0.0", "")]);
		let mut item = select("a", "1.0.0");
		item.enabled = false;
		let resolution = plan(&game(), &store, &[item]);
		assert!(resolution.is_launchable());
		assert!(resolution.order.is_empty());
	}

	#[test]
	fn parses_eu4_launcher_versions() {
		assert_eq!(
			parse_game_version("v1.37.5.0"),
			Some(Version::new(1, 37, 5))
		);
		assert_eq!(parse_game_version("1.37"), Some(Version::new(1, 37, 0)));
		assert_eq!(parse_game_version("garbage"), None);
	}

	#[test]
	fn same_file_different_digest_collides() {
		let a = manifest(
			"a",
			"1.0.0",
			"[[files]]\npath = \"plugins/shared.dll\"\nsha256 = \"aa\"",
		);
		let b = manifest(
			"b",
			"1.0.0",
			"[[files]]\npath = \"plugins/shared.dll\"\nsha256 = \"bb\"",
		);
		let store = store(vec![a, b]);
		let resolution = plan(
			&game(),
			&store,
			&[select("a", "1.0.0"), select("b", "1.0.0")],
		);
		assert!(
			resolution
				.errors
				.iter()
				.any(|d| matches!(d, Diagnostic::FileCollision { .. }))
		);
	}

	#[test]
	fn startup_phases_take_precedence_and_impossible_constraints_are_refused() {
		let deferred = manifest("a-deferred", "1.0.0", "");
		let mut entry = manifest("z-entry", "1.0.0", "");
		entry.entry.phase = Phase::Entry;
		let selections = [select("a-deferred", "1.0.0"), select("z-entry", "1.0.0")];
		let ordered = plan(
			&game(),
			&store(vec![deferred.clone(), entry.clone()]),
			&selections,
		);
		assert!(ordered.is_launchable());
		assert_eq!(ordered.order[0].id, "z-entry");
		entry.load_after.push("a-deferred".into());
		assert!(!plan(&game(), &store(vec![deferred, entry]), &selections).is_launchable());
	}

	#[test]
	fn dll_basename_collisions_apply_across_different_package_directories() {
		let a = manifest(
			"a",
			"1.0.0",
			"[[files]]\npath=\"one/util.dll\"\nsha256=\"aa\"",
		);
		let b = manifest(
			"b",
			"1.0.0",
			"[[files]]\npath=\"two/UTIL.DLL\"\nsha256=\"bb\"",
		);
		assert!(
			!plan(
				&game(),
				&store(vec![a, b]),
				&[select("a", "1.0.0"), select("b", "1.0.0")]
			)
			.is_launchable()
		);
	}

	#[test]
	fn identical_entry_names_are_refused_but_identical_dependencies_are_allowed() {
		let a = manifest(
			"a",
			"1.0.0",
			"[[files]]\npath=\"shared.dll\"\nsha256=\"aa\"",
		);
		let mut b = manifest(
			"b",
			"1.0.0",
			"[[files]]\npath=\"shared.dll\"\nsha256=\"aa\"",
		);
		let selections = [select("a", "1.0.0"), select("b", "1.0.0")];
		assert!(plan(&game(), &store(vec![a.clone(), b.clone()]), &selections).is_launchable());
		b.entry.path = format!("other/{}", a.entry.path.to_uppercase());
		assert!(!plan(&game(), &store(vec![a, b]), &selections).is_launchable());
	}

	#[test]
	fn one_way_conflicts_are_enforced_in_both_lexical_directions() {
		let a = manifest("a", "1.0.0", "");
		let z = manifest("z", "1.0.0", "[[conflicts]]\nid=\"a\"\nversion=\"*\"");
		let resolution = plan(
			&game(),
			&store(vec![a, z]),
			&[select("a", "1.0.0"), select("z", "1.0.0")],
		);
		assert!(!resolution.is_launchable());
		assert_eq!(
			resolution
				.errors
				.iter()
				.filter(|d| matches!(d, Diagnostic::Conflict { .. }))
				.count(),
			1
		);
	}
}
