use super::content_family::{GameProfile, MergeKeySource};
use super::eu4_profile::eu4_profile;
use super::semantic_index::{count_symbol_references_resolving_to_mod, is_decision_container_key};
use foch_core::model::{
	CheckContext, DepMisuseEvidence, DepMisuseFinding, Finding, FindingChannel, ModCandidate,
	ScopeKind, ScopeNode, SemanticIndex, Severity, SymbolKind, VersionMismatchFinding,
};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn check_required_fields(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();

	for (idx, entry) in ctx.playlist.mods.iter().enumerate() {
		let mod_id = entry.steam_id.clone();

		if entry
			.display_name
			.as_deref()
			.map(str::trim)
			.unwrap_or("")
			.is_empty()
		{
			findings.push(new_finding(
				"missing-playset-field",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod entry {idx} missing displayName"),
				mod_id.clone(),
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}

		if entry
			.steam_id
			.as_deref()
			.map(str::trim)
			.unwrap_or("")
			.is_empty()
		{
			findings.push(new_finding(
				"missing-playset-field",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod entry {idx} missing steamId"),
				None,
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}

		if entry.position.is_none() {
			findings.push(new_finding(
				"missing-playset-field",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod entry {idx} missing position"),
				mod_id,
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}
	}

	findings
}

pub fn check_duplicate_mod_identity(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut seen_steam_ids = HashMap::<String, usize>::new();
	let mut seen_positions = HashMap::<usize, usize>::new();

	for (idx, entry) in ctx.playlist.mods.iter().enumerate() {
		if let Some(steam_id) = entry.steam_id.as_ref() {
			if let Some(first_idx) = seen_steam_ids.get(steam_id) {
				findings.push(new_finding(
					"duplicate-playset-entry",
					Severity::Error,
					FindingChannel::Strict,
					format!("steamId conflict: {steam_id} (first seen at index {first_idx})"),
					Some(steam_id.clone()),
					Some(ctx.playlist_path.clone()),
					Some(format!("duplicate entry index: {idx}")),
					None,
					None,
					Some(1.0),
				));
			} else {
				seen_steam_ids.insert(steam_id.clone(), idx);
			}
		}

		if let Some(position) = entry.position {
			if let Some(first_idx) = seen_positions.get(&position) {
				findings.push(new_finding(
					"duplicate-playset-entry",
					Severity::Error,
					FindingChannel::Strict,
					format!("position conflict: {position} (first seen at index {first_idx})"),
					entry.steam_id.clone(),
					Some(ctx.playlist_path.clone()),
					Some(format!("duplicate entry index: {idx}")),
					None,
					None,
					Some(1.0),
				));
			} else {
				seen_positions.insert(position, idx);
			}
		}
	}

	findings
}

pub fn check_missing_descriptor(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();

	for mod_item in &ctx.mods {
		if !mod_item.entry.enabled {
			continue;
		}

		match (
			&mod_item.descriptor_path,
			&mod_item.descriptor,
			&mod_item.descriptor_error,
		) {
			(None, _, _) => findings.push(new_finding(
				"mod-descriptor-error",
				Severity::Error,
				FindingChannel::Strict,
				"failed to locate descriptor.mod".to_string(),
				Some(mod_item.mod_id.clone()),
				mod_item.root_path.clone(),
				None,
				None,
				None,
				Some(1.0),
			)),
			(Some(path), None, Some(err)) => findings.push(new_finding(
				"mod-descriptor-error",
				Severity::Error,
				FindingChannel::Strict,
				"failed to parse descriptor.mod".to_string(),
				Some(mod_item.mod_id.clone()),
				Some(path.clone()),
				Some(err.clone()),
				None,
				None,
				Some(1.0),
			)),
			(Some(path), None, None) => findings.push(new_finding(
				"mod-descriptor-error",
				Severity::Error,
				FindingChannel::Strict,
				"descriptor.mod does not exist".to_string(),
				Some(mod_item.mod_id.clone()),
				Some(path.clone()),
				None,
				None,
				None,
				Some(1.0),
			)),
			_ => {}
		}
	}

	findings
}

pub fn check_file_conflict(ctx: &CheckContext) -> Vec<Finding> {
	let mut file_owners: HashMap<String, Vec<String>> = HashMap::new();
	for mod_item in &ctx.mods {
		for file in &mod_item.files {
			let key = file.to_string_lossy().to_string();
			file_owners
				.entry(key)
				.or_default()
				.push(mod_item.mod_id.clone());
		}
	}

	let mut findings = Vec::new();
	for (path, owners) in file_owners {
		let unique: Vec<String> = {
			let mut seen = HashSet::new();
			owners
				.into_iter()
				.filter(|owner| seen.insert(owner.clone()))
				.collect()
		};

		if unique.len() < 2 {
			continue;
		}

		let mergeable = is_structurally_mergeable_path(&path);
		let message = if mergeable {
			format!("file overwrite conflict (structural auto-merge candidate): {path}")
		} else {
			format!("file overwrite conflict: {path}")
		};
		let evidence = if mergeable {
			format!("{} | merge_hint=structural", unique.join(" -> "))
		} else {
			unique.join(" -> ")
		};

		findings.push(new_finding(
			"file-overwrite-conflict",
			Severity::Warning,
			FindingChannel::Advisory,
			message,
			unique.last().cloned(),
			Some(path.into()),
			Some(evidence),
			None,
			None,
			Some(if mergeable { 0.75 } else { 0.85 }),
		));
	}

	findings
}

fn is_structurally_mergeable_path(path: &str) -> bool {
	let normalized = path.replace('\\', "/").to_ascii_lowercase();
	if normalized.ends_with(".gui") || normalized.ends_with(".gfx") {
		return normalized.starts_with("interface/")
			|| normalized.starts_with("common/interface/")
			|| normalized.starts_with("gfx/");
	}
	if normalized.ends_with(".txt") || normalized.ends_with(".lua") {
		return normalized.starts_with("events/")
			|| normalized.starts_with("decisions/")
			|| normalized.starts_with("common/scripted_effects/")
			|| normalized.starts_with("common/diplomatic_actions/")
			|| normalized.starts_with("common/triggered_modifiers/")
			|| normalized.starts_with("common/defines/")
			|| normalized.starts_with("interface/")
			|| normalized.starts_with("common/interface/");
	}
	false
}

pub fn check_missing_dependency(ctx: &CheckContext) -> Vec<Finding> {
	let identity = foch_core::domain::dep_resolution::ModIdentityIndex::from_mods(&ctx.mods);

	let mut findings = Vec::new();
	for mod_item in &ctx.mods {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			if identity.contains(dependency) {
				continue;
			}

			findings.push(new_finding(
				"missing-mod-dependency",
				Severity::Warning,
				FindingChannel::Advisory,
				"missing dependency".to_string(),
				Some(mod_item.mod_id.clone()),
				mod_item.descriptor_path.clone(),
				Some(format!("{} depends on {dependency}", descriptor.name)),
				None,
				None,
				Some(0.9),
			));
		}
	}

	findings
}

pub fn detect_dependency_misuse(ctx: &CheckContext) -> Vec<DepMisuseFinding> {
	let identity = foch_core::domain::dep_resolution::ModIdentityIndex::from_mods(&ctx.mods);
	let semantic_signals = DependencySemanticSignals::from_context(ctx);
	let mut findings = Vec::new();

	for mod_item in ctx.mods.iter().filter(|item| item.entry.enabled) {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			let Some(dep_idx) = identity.lookup(dependency) else {
				continue;
			};
			let Some(dep_mod) = ctx.mods.get(dep_idx) else {
				continue;
			};
			if !dep_mod.entry.enabled || dep_mod.mod_id == mod_item.mod_id {
				continue;
			}

			let semantic_refs_to_dep = count_symbol_references_resolving_to_mod(
				&ctx.semantic_index,
				&mod_item.mod_id,
				&dep_mod.mod_id,
			);
			if has_declared_dependency_semantic_signal(
				&semantic_signals,
				mod_item,
				dep_mod,
				semantic_refs_to_dep,
			) {
				continue;
			}

			findings.push(DepMisuseFinding {
				mod_id: mod_item.mod_id.clone(),
				mod_display_name: display_name_for_mod(mod_item),
				suspicious_dep_id: dep_mod.mod_id.clone(),
				suspicious_dep_display_name: display_name_for_mod(dep_mod),
				evidence: DepMisuseEvidence {
					semantic_refs_to_dep,
					false_remove_count: 0,
				},
			});
		}
	}

	findings
}

#[derive(Default)]
struct DependencySemanticSignals {
	localisation_keys_by_mod: HashMap<String, HashSet<String>>,
	family_keys_by_mod: HashMap<String, HashSet<(String, String)>>,
}

impl DependencySemanticSignals {
	fn from_context(ctx: &CheckContext) -> Self {
		let mut signals = Self::default();

		for definition in &ctx.semantic_index.localisation_definitions {
			signals
				.localisation_keys_by_mod
				.entry(definition.mod_id.clone())
				.or_default()
				.insert(definition.key.clone());
		}

		signals.family_keys_by_mod = collect_content_family_merge_keys(&ctx.semantic_index);
		signals
	}

	fn has_localisation_key_overlap(&self, mod_id: &str, dep_mod_id: &str) -> bool {
		let (Some(mod_keys), Some(dep_keys)) = (
			self.localisation_keys_by_mod.get(mod_id),
			self.localisation_keys_by_mod.get(dep_mod_id),
		) else {
			return false;
		};
		sets_overlap(mod_keys, dep_keys)
	}

	fn has_content_family_merge_key_overlap(&self, mod_id: &str, dep_mod_id: &str) -> bool {
		let (Some(mod_keys), Some(dep_keys)) = (
			self.family_keys_by_mod.get(mod_id),
			self.family_keys_by_mod.get(dep_mod_id),
		) else {
			return false;
		};
		sets_overlap(mod_keys, dep_keys)
	}
}

fn collect_content_family_merge_keys(
	index: &SemanticIndex,
) -> HashMap<String, HashSet<(String, String)>> {
	let mut keys_by_mod = HashMap::<String, HashSet<(String, String)>>::new();
	let scalar_values = scalar_values_by_scope(index);

	for scope in &index.scopes {
		let Some(parent) = parent_scope(index, scope) else {
			continue;
		};
		let Some((family_id, merge_key_source)) = dependency_merge_key_source_for_path(&scope.path)
		else {
			continue;
		};

		match merge_key_source {
			MergeKeySource::AssignmentKey if parent.kind == ScopeKind::File => {
				insert_family_key(
					&mut keys_by_mod,
					&scope.mod_id,
					family_id,
					scope.key.clone(),
				);
			}
			MergeKeySource::FieldValue(field) if parent.kind == ScopeKind::File => {
				if let Some(value) = scalar_values.get(&(scope.id, field.to_string())) {
					insert_family_key(&mut keys_by_mod, &scope.mod_id, family_id, value.clone());
				}
			}
			MergeKeySource::ContainerChildKey => {
				if is_container_child_scope(index, parent) {
					insert_family_key(
						&mut keys_by_mod,
						&scope.mod_id,
						family_id,
						scope.key.clone(),
					);
				}
			}
			MergeKeySource::ContainerChildFieldValue {
				container,
				child_key_field,
				child_types,
			} => {
				if parent.kind == ScopeKind::File {
					if scope.key != container {
						insert_family_key(
							&mut keys_by_mod,
							&scope.mod_id,
							family_id,
							scope.key.clone(),
						);
					}
				} else if parent.key == container && parent_parent_is_file(index, parent) {
					let key = if (child_types.is_empty()
						|| child_types.contains(&scope.key.as_str()))
						&& let Some(value) =
							scalar_values.get(&(scope.id, child_key_field.to_string()))
					{
						format!("{}:{value}", scope.key)
					} else {
						scope.key.clone()
					};
					insert_family_key(&mut keys_by_mod, &scope.mod_id, family_id, key);
				}
			}
			_ => {}
		}
	}

	keys_by_mod
}

fn dependency_merge_key_source_for_path(path: &Path) -> Option<(&'static str, MergeKeySource)> {
	let descriptor = eu4_profile().classify_content_family(path)?;
	let source = descriptor.merge_key_source?;
	is_dependency_merge_key_source(source).then_some((descriptor.id, source))
}

fn scalar_values_by_scope(index: &SemanticIndex) -> HashMap<(usize, String), String> {
	let mut values = HashMap::new();
	for assignment in &index.scalar_assignments {
		values
			.entry((assignment.scope_id, assignment.key.clone()))
			.or_insert_with(|| assignment.value.clone());
	}
	values
}

fn insert_family_key(
	keys_by_mod: &mut HashMap<String, HashSet<(String, String)>>,
	mod_id: &str,
	family_id: &str,
	key: String,
) {
	if key.is_empty() {
		return;
	}
	keys_by_mod
		.entry(mod_id.to_string())
		.or_default()
		.insert((family_id.to_string(), key));
}

fn parent_scope<'a>(index: &'a SemanticIndex, scope: &ScopeNode) -> Option<&'a ScopeNode> {
	index.scopes.get(scope.parent?)
}

fn parent_parent_is_file(index: &SemanticIndex, scope: &ScopeNode) -> bool {
	parent_scope(index, scope).is_some_and(|parent| parent.kind == ScopeKind::File)
}

fn is_container_child_scope(index: &SemanticIndex, parent: &ScopeNode) -> bool {
	is_decision_container_key(&parent.key) && parent_parent_is_file(index, parent)
}

fn has_declared_dependency_semantic_signal(
	signals: &DependencySemanticSignals,
	mod_item: &ModCandidate,
	dep_mod: &ModCandidate,
	semantic_refs_to_dep: u32,
) -> bool {
	semantic_refs_to_dep > 0
		|| signals.has_localisation_key_overlap(&mod_item.mod_id, &dep_mod.mod_id)
		|| signals.has_content_family_merge_key_overlap(&mod_item.mod_id, &dep_mod.mod_id)
		|| replace_path_covers_dependency_content(mod_item, dep_mod)
}

fn replace_path_covers_dependency_content(mod_item: &ModCandidate, dep_mod: &ModCandidate) -> bool {
	let Some(descriptor) = mod_item.descriptor.as_ref() else {
		return false;
	};
	let prefixes: Vec<String> = descriptor
		.replace_path
		.iter()
		.map(|path| normalize_path_prefix(path))
		.filter(|path| !path.is_empty())
		.collect();
	if prefixes.is_empty() {
		return false;
	}

	dep_mod.files.iter().any(|file| {
		let normalized = normalize_path_prefix(&normalize_relative_path(file));
		prefixes
			.iter()
			.any(|prefix| path_is_under_prefix(&normalized, prefix))
	})
}

fn normalize_path_prefix(raw: &str) -> String {
	raw.trim().trim_matches('/').replace('\\', "/")
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn path_is_under_prefix(normalized_file: &str, prefix: &str) -> bool {
	normalized_file == prefix || normalized_file.starts_with(&format!("{prefix}/"))
}

fn is_dependency_merge_key_source(source: MergeKeySource) -> bool {
	matches!(
		source,
		MergeKeySource::AssignmentKey
			| MergeKeySource::FieldValue(_)
			| MergeKeySource::ContainerChildKey
			| MergeKeySource::ContainerChildFieldValue { .. }
	)
}

fn sets_overlap<T>(lhs: &HashSet<T>, rhs: &HashSet<T>) -> bool
where
	T: Eq + std::hash::Hash,
{
	if lhs.len() <= rhs.len() {
		lhs.iter().any(|item| rhs.contains(item))
	} else {
		rhs.iter().any(|item| lhs.contains(item))
	}
}

pub fn detect_version_mismatch(
	ctx: &CheckContext,
	game_version: &str,
) -> Vec<VersionMismatchFinding> {
	let Some(vanilla_version) = parse_game_version(game_version) else {
		return Vec::new();
	};
	let game_version = game_version.trim();

	let mut findings = Vec::new();
	for mod_item in ctx.mods.iter().filter(|item| item.entry.enabled) {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};
		let Some(supported_version) = descriptor.supported_version.as_deref() else {
			continue;
		};
		let supported_version = supported_version.trim();
		if supported_version.is_empty() || supported_version == "*" {
			continue;
		}

		let Some(parsed_supported_version) = parse_game_version(supported_version) else {
			continue;
		};
		let severity = match parsed_supported_version.cmp_major_minor(&vanilla_version) {
			Ordering::Less => Severity::Info,
			Ordering::Equal => continue,
			Ordering::Greater => Severity::Warning,
		};
		let message = match severity {
			Severity::Info => "mod targets older game version, may have stale references",
			Severity::Warning => {
				"mod targets newer game version (likely beta branch), may use unsupported features"
			}
			Severity::Error => unreachable!("version mismatch never emits error severity"),
		};

		findings.push(VersionMismatchFinding {
			tag: "version_mismatch".to_string(),
			severity,
			mod_id: mod_item.mod_id.clone(),
			mod_display_name: display_name_for_mod(mod_item),
			supported_version: supported_version.to_string(),
			game_version: game_version.to_string(),
			message: message.to_string(),
		});
	}

	findings
}

pub fn check_version_mismatch(ctx: &CheckContext, game_version: &str) -> Vec<Finding> {
	detect_version_mismatch(ctx, game_version)
		.into_iter()
		.map(|finding| {
			new_finding(
				"mod-version-mismatch",
				finding.severity,
				FindingChannel::Advisory,
				finding.message.clone(),
				Some(finding.mod_id.clone()),
				ctx.mods
					.iter()
					.find(|mod_item| mod_item.mod_id == finding.mod_id)
					.and_then(|mod_item| mod_item.descriptor_path.clone()),
				Some(format!(
					"tag={} supported_version={} game_version={}",
					finding.tag, finding.supported_version, finding.game_version
				)),
				None,
				None,
				Some(0.8),
			)
		})
		.collect()
}

pub fn check_dependency_misuse(ctx: &CheckContext) -> Vec<Finding> {
	detect_dependency_misuse(ctx)
		.into_iter()
		.map(|finding| {
			new_finding(
				"unused-mod-dependency",
				Severity::Warning,
				FindingChannel::Advisory,
				format!(
					"descriptor.mod dependencies misuse suspected: {} declares {} without semantic references",
					finding.mod_id, finding.suspicious_dep_id
				),
				Some(finding.mod_id.clone()),
				ctx.mods
					.iter()
					.find(|mod_item| mod_item.mod_id == finding.mod_id)
					.and_then(|mod_item| mod_item.descriptor_path.clone()),
				Some(format!(
					"dep_id={} dep_display_name={} semantic_refs_to_dep={} false_remove_count={}",
					finding.suspicious_dep_id,
					finding.suspicious_dep_display_name,
					finding.evidence.semantic_refs_to_dep,
					finding.evidence.false_remove_count
				)),
				None,
				None,
				Some(0.8),
			)
		})
		.collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedGameVersion {
	major: u32,
	minor: u32,
}

impl ParsedGameVersion {
	fn cmp_major_minor(&self, other: &Self) -> Ordering {
		self.major
			.cmp(&other.major)
			.then_with(|| self.minor.cmp(&other.minor))
	}
}

fn parse_game_version(value: &str) -> Option<ParsedGameVersion> {
	let value = value.trim().trim_matches('"').trim();
	if value.is_empty() || value == "*" {
		return None;
	}
	let value = value
		.strip_prefix('v')
		.or_else(|| value.strip_prefix('V'))
		.unwrap_or(value);

	let mut parts = value.split('.');
	let major = parse_version_component(parts.next()?)?;
	let minor = parse_version_component(parts.next()?)?;
	if let Some(patch) = parts.next() {
		let patch = patch.trim();
		if patch != "*" {
			parse_version_component(patch)?;
		}
	}
	if parts.next().is_some() {
		return None;
	}

	Some(ParsedGameVersion { major, minor })
}

fn parse_version_component(value: &str) -> Option<u32> {
	let value = value.trim();
	if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
		return None;
	}
	value.parse().ok()
}

fn display_name_for_mod(mod_item: &foch_core::model::ModCandidate) -> String {
	mod_item
		.entry
		.display_name
		.as_deref()
		.map(str::trim)
		.filter(|name| !name.is_empty())
		.or_else(|| {
			mod_item
				.descriptor
				.as_ref()
				.map(|descriptor| descriptor.name.trim())
				.filter(|name| !name.is_empty())
		})
		.unwrap_or(mod_item.mod_id.as_str())
		.to_string()
}

pub fn check_duplicate_scripted_effect(ctx: &CheckContext) -> Vec<Finding> {
	let mut grouped: HashMap<&str, Vec<_>> = HashMap::new();
	for definition in &ctx.semantic_index.definitions {
		if definition.kind == SymbolKind::ScriptedEffect {
			grouped
				.entry(&definition.name)
				.or_default()
				.push(definition);
		}
	}

	let mut findings = Vec::new();
	for (name, defs) in grouped {
		let mut unique_mods = HashSet::new();
		for def in &defs {
			unique_mods.insert(def.mod_id.as_str());
		}
		if unique_mods.len() < 2 {
			continue;
		}

		let evidence = defs
			.iter()
			.map(|def| format!("{}:{}#L{}", def.mod_id, def.path.display(), def.line))
			.collect::<Vec<_>>()
			.join("; ");
		let Some(last) = defs.last() else {
			continue;
		};
		findings.push(new_finding(
			"duplicate-scripted-effect",
			Severity::Warning,
			FindingChannel::Advisory,
			format!("duplicate scripted effect: {name}"),
			Some(last.mod_id.clone()),
			Some(last.path.clone()),
			Some(evidence),
			Some(last.line),
			Some(last.column),
			Some(0.8),
		));
	}

	findings
}

#[allow(clippy::too_many_arguments)]
fn new_finding(
	rule_id: &str,
	severity: Severity,
	channel: FindingChannel,
	message: String,
	mod_id: Option<String>,
	path: Option<std::path::PathBuf>,
	evidence: Option<String>,
	line: Option<usize>,
	column: Option<usize>,
	confidence: Option<f32>,
) -> Finding {
	Finding {
		rule_id: rule_id.to_string(),
		severity,
		channel,
		message,
		mod_id,
		path,
		evidence,
		line,
		column,
		confidence,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analyzer::semantic_index::{build_semantic_index, parse_script_file};
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::{Playlist, PlaylistEntry};
	use foch_core::model::{
		LocalisationDefinition, ModCandidate, ScopeType, SemanticIndex, SymbolDefinition,
		SymbolReference,
	};
	use std::fs;
	use std::path::{Path, PathBuf};

	fn candidate(
		mod_id: &str,
		display_name: &str,
		descriptor_name: &str,
		deps: &[&str],
	) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				display_name: Some(display_name.to_string()),
				enabled: true,
				position: Some(0),
				steam_id: Some(mod_id.to_string()),
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: Some(PathBuf::from(format!("{mod_id}/descriptor.mod"))),
			descriptor: Some(ModDescriptor {
				name: descriptor_name.to_string(),
				dependencies: deps.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn version_candidate(mod_id: &str, supported_version: Option<&str>) -> ModCandidate {
		let mut mod_item = candidate(mod_id, "Versioned Mod", "Versioned Mod", &[]);
		if let Some(descriptor) = mod_item.descriptor.as_mut() {
			descriptor.supported_version = supported_version.map(str::to_string);
		}
		mod_item
	}

	fn definition(mod_id: &str, local_name: &str) -> SymbolDefinition {
		SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: format!("eu4::common.scripted_effects::{local_name}"),
			module: "common.scripted_effects".to_string(),
			local_name: local_name.to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Unknown,
			inferred_this_type: ScopeType::Unknown,
			inferred_this_mask: 0,
			inferred_from_mask: 0,
			inferred_root_mask: 0,
			required_params: Vec::new(),
			optional_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		}
	}

	fn reference(mod_id: &str, name: &str) -> SymbolReference {
		SymbolReference {
			kind: SymbolKind::ScriptedEffect,
			name: name.to_string(),
			module: "common.scripted_effects".to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from("common/scripted_effects/caller.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			provided_params: Vec::new(),
			param_bindings: Vec::new(),
		}
	}

	fn context(mods: Vec<ModCandidate>, semantic_index: SemanticIndex) -> CheckContext {
		CheckContext {
			playlist_path: PathBuf::from("playlist.json"),
			playlist: Playlist {
				mods: mods.iter().map(|mod_item| mod_item.entry.clone()).collect(),
				..Playlist::default()
			},
			mods,
			semantic_index,
		}
	}

	fn with_files(mut mod_item: ModCandidate, root: PathBuf, files: &[&str]) -> ModCandidate {
		mod_item.root_path = Some(root);
		mod_item.files = files.iter().map(PathBuf::from).collect();
		mod_item
	}

	fn write_fixture(root: &Path, relative: &str, contents: &str) {
		let path = root.join(relative);
		fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture dirs");
		fs::write(path, contents).expect("fixture file");
	}

	fn tempdir_in_target() -> tempfile::TempDir {
		let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/rules-tests");
		fs::create_dir_all(&target).expect("rules test target dir");
		tempfile::Builder::new()
			.prefix("dep-misuse-")
			.tempdir_in(target)
			.expect("rules test temp dir")
	}

	fn localisation_definition(mod_id: &str, key: &str) -> LocalisationDefinition {
		LocalisationDefinition {
			key: key.to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from("localisation/test_l_english.yml"),
			line: 2,
			column: 2,
		}
	}

	#[test]
	fn supported_version_wildcard_matching_minor_has_no_finding() {
		let ctx = context(
			vec![version_candidate("100", Some("1.37.*"))],
			SemanticIndex::default(),
		);

		let findings = detect_version_mismatch(&ctx, "1.37.5");

		assert!(findings.is_empty());
	}

	#[test]
	fn supported_version_older_minor_is_info() {
		let ctx = context(
			vec![version_candidate("100", Some("1.36.*"))],
			SemanticIndex::default(),
		);

		let findings = detect_version_mismatch(&ctx, "1.37.5");

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].tag, "version_mismatch");
		assert_eq!(findings[0].severity, Severity::Info);
		assert_eq!(findings[0].supported_version, "1.36.*");
		assert_eq!(findings[0].game_version, "1.37.5");
		assert_eq!(
			check_version_mismatch(&ctx, "1.37.5")[0].rule_id,
			"mod-version-mismatch"
		);
	}

	#[test]
	fn supported_version_newer_minor_is_warning() {
		let ctx = context(
			vec![version_candidate("100", Some("1.38.*"))],
			SemanticIndex::default(),
		);

		let findings = detect_version_mismatch(&ctx, "1.37.5");

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].severity, Severity::Warning);
		assert!(findings[0].message.contains("newer game version"));
	}

	#[test]
	fn loose_supported_version_is_ignored() {
		let ctx = context(
			vec![
				version_candidate("100", None),
				version_candidate("200", Some("*")),
			],
			SemanticIndex::default(),
		);

		let findings = detect_version_mismatch(&ctx, "1.37.5");

		assert!(findings.is_empty());
	}

	#[test]
	fn flags_declared_dependency_with_no_semantic_refs() {
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Dependency Mod"]),
				candidate("200", "Dependency Mod", "Dependency Mod", &[]),
			],
			SemanticIndex::default(),
		);

		let findings = detect_dependency_misuse(&ctx);

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].mod_id, "100");
		assert_eq!(findings[0].suspicious_dep_id, "200");
		assert_eq!(findings[0].evidence.semantic_refs_to_dep, 0);
		assert_eq!(
			check_dependency_misuse(&ctx)[0].rule_id,
			"unused-mod-dependency"
		);
	}

	#[test]
	fn does_not_flag_dependency_with_semantic_ref() {
		let mut index = SemanticIndex::default();
		index.definitions.push(definition("200", "shared_effect"));
		index.references.push(reference("100", "shared_effect"));
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Dependency Mod"]),
				candidate("200", "Dependency Mod", "Dependency Mod", &[]),
			],
			index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	#[test]
	fn does_not_flag_dependency_with_localisation_key_overlap() {
		let mut index = SemanticIndex::default();
		index
			.localisation_definitions
			.push(localisation_definition("100", "shared_loc_key"));
		index
			.localisation_definitions
			.push(localisation_definition("200", "shared_loc_key"));
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Dependency Mod"]),
				candidate("200", "Dependency Mod", "Dependency Mod", &[]),
			],
			index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	#[test]
	fn does_not_flag_dependency_with_content_family_merge_key_overlap() {
		let tempdir = tempdir_in_target();
		let main_root = tempdir.path().join("main");
		let dep_root = tempdir.path().join("dep");
		let relative = "common/static_modifiers/shared.txt";
		write_fixture(
			&main_root,
			relative,
			"shared_modifier = { global_tax_modifier = 0.20 }\n",
		);
		write_fixture(
			&dep_root,
			relative,
			"shared_modifier = { global_tax_modifier = 0.10 }\n",
		);
		let parsed = vec![
			parse_script_file("100", &main_root, &main_root.join(relative)).expect("main parsed"),
			parse_script_file("200", &dep_root, &dep_root.join(relative)).expect("dep parsed"),
		];
		let semantic_index = build_semantic_index(&parsed);
		let ctx = context(
			vec![
				with_files(
					candidate("100", "Main Mod", "main", &["Dependency Mod"]),
					main_root,
					&[relative],
				),
				with_files(
					candidate("200", "Dependency Mod", "Dependency Mod", &[]),
					dep_root,
					&[relative],
				),
			],
			semantic_index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	#[test]
	fn does_not_flag_dependency_with_replace_path_coverage() {
		let mut main = candidate("100", "Main Mod", "main", &["Dependency Mod"]);
		main.descriptor.as_mut().unwrap().replace_path = vec!["common/missions".to_string()];
		let mut dep = candidate("200", "Dependency Mod", "Dependency Mod", &[]);
		dep.files = vec![PathBuf::from("common/missions/dep_missions.txt")];
		let ctx = context(vec![main, dep], SemanticIndex::default());

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	#[test]
	fn flags_only_unused_declared_dependency() {
		let mut index = SemanticIndex::default();
		index.definitions.push(definition("200", "used_effect"));
		index.definitions.push(definition("300", "unused_effect"));
		index.references.push(reference("100", "used_effect"));
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Used Mod", "Unused Mod"]),
				candidate("200", "Used Mod", "Used Mod", &[]),
				candidate("300", "Unused Mod", "Unused Mod", &[]),
			],
			index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].suspicious_dep_id, "300");
	}
}
