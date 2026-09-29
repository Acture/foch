use super::super::content::{MergeKeySource, eu4};
use super::super::script::{count_symbol_references_resolving_to_mod, is_decision_container_key};
use crate::model::{
	CheckContext, DepMisuseEvidence, DepMisuseFinding, Finding, FindingChannel, GamePath,
	GamePathBuf, ModCandidate, ScopeKind, ScopeNode, SemanticIndex, Severity, SymbolKind,
	VersionMismatchFinding,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

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
			findings.push(new_finding(FindingArgs {
				rule_id: "missing-playset-field",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: format!("mod entry {idx} missing displayName"),
				mod_id: mod_id.clone(),
				path: None,
				source_file: Some(ctx.playlist_path.clone()),
				evidence: None,
				line: None,
				column: None,
				confidence: Some(1.0),
			}));
		}

		if entry
			.steam_id
			.as_deref()
			.map(str::trim)
			.unwrap_or("")
			.is_empty()
		{
			findings.push(new_finding(FindingArgs {
				rule_id: "missing-playset-field",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: format!("mod entry {idx} missing steamId"),
				mod_id: None,
				path: None,
				source_file: Some(ctx.playlist_path.clone()),
				evidence: None,
				line: None,
				column: None,
				confidence: Some(1.0),
			}));
		}

		if entry.position.is_none() {
			findings.push(new_finding(FindingArgs {
				rule_id: "missing-playset-field",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: format!("mod entry {idx} missing position"),
				mod_id,
				path: None,
				source_file: Some(ctx.playlist_path.clone()),
				evidence: None,
				line: None,
				column: None,
				confidence: Some(1.0),
			}));
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
				findings.push(new_finding(FindingArgs {
					rule_id: "duplicate-playset-entry",
					severity: Severity::Error,
					channel: FindingChannel::Strict,
					message: format!(
						"steamId conflict: {steam_id} (first seen at index {first_idx})"
					),
					mod_id: Some(steam_id.clone()),
					path: None,
					source_file: Some(ctx.playlist_path.clone()),
					evidence: Some(format!("duplicate entry index: {idx}")),
					line: None,
					column: None,
					confidence: Some(1.0),
				}));
			} else {
				seen_steam_ids.insert(steam_id.clone(), idx);
			}
		}

		if let Some(position) = entry.position {
			if let Some(first_idx) = seen_positions.get(&position) {
				findings.push(new_finding(FindingArgs {
					rule_id: "duplicate-playset-entry",
					severity: Severity::Error,
					channel: FindingChannel::Strict,
					message: format!(
						"position conflict: {position} (first seen at index {first_idx})"
					),
					mod_id: entry.steam_id.clone(),
					path: None,
					source_file: Some(ctx.playlist_path.clone()),
					evidence: Some(format!("duplicate entry index: {idx}")),
					line: None,
					column: None,
					confidence: Some(1.0),
				}));
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
			(None, _, _) => findings.push(new_finding(FindingArgs {
				rule_id: "mod-descriptor-error",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: "failed to locate descriptor.mod".to_string(),
				mod_id: Some(mod_item.mod_id.clone()),
				path: None,
				source_file: mod_item.root_path.clone(),
				evidence: None,
				line: None,
				column: None,
				confidence: Some(1.0),
			})),
			(Some(path), None, Some(err)) => findings.push(new_finding(FindingArgs {
				rule_id: "mod-descriptor-error",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: "failed to parse descriptor.mod".to_string(),
				mod_id: Some(mod_item.mod_id.clone()),
				path: None,
				source_file: Some(path.clone()),
				evidence: Some(err.clone()),
				line: None,
				column: None,
				confidence: Some(1.0),
			})),
			(Some(path), None, None) => findings.push(new_finding(FindingArgs {
				rule_id: "mod-descriptor-error",
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: "descriptor.mod does not exist".to_string(),
				mod_id: Some(mod_item.mod_id.clone()),
				path: None,
				source_file: Some(path.clone()),
				evidence: None,
				line: None,
				column: None,
				confidence: Some(1.0),
			})),
			_ => {}
		}
	}

	findings
}

pub fn check_file_conflict(ctx: &CheckContext) -> Vec<Finding> {
	// Ordered by game path, so findings come out in a stable order.
	let mut file_owners: BTreeMap<&GamePath, Vec<&str>> = BTreeMap::new();
	for mod_item in &ctx.mods {
		for file in &mod_item.files {
			file_owners
				.entry(file)
				.or_default()
				.push(mod_item.mod_id.as_str());
		}
	}

	let mut findings = Vec::new();
	for (path, owners) in file_owners {
		let unique: Vec<&str> = {
			let mut seen = HashSet::new();
			owners
				.into_iter()
				.filter(|owner| seen.insert(*owner))
				.collect()
		};

		if unique.len() < 2 {
			continue;
		}

		let mergeable = is_structurally_mergeable_path(path);
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

		findings.push(new_finding(FindingArgs {
			rule_id: "file-overwrite-conflict",
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message,
			mod_id: unique.last().map(|owner| (*owner).to_string()),
			path: Some(path.to_owned()),
			source_file: None,
			evidence: Some(evidence),
			line: None,
			column: None,
			confidence: Some(if mergeable { 0.75 } else { 0.85 }),
		}));
	}

	findings
}

/// Names compare ignoring ASCII case.
fn is_structurally_mergeable_path(path: &GamePath) -> bool {
	let inside_any = |directories: &[&[&str]]| {
		directories
			.iter()
			.any(|directory| path.is_inside(directory, str::eq_ignore_ascii_case))
	};
	let has_extension = |extensions: &[&str]| {
		path.extension().is_some_and(|extension| {
			extensions
				.iter()
				.any(|expected| extension.eq_ignore_ascii_case(expected))
		})
	};
	if has_extension(&["gui", "gfx"]) {
		return inside_any(&[&["interface"], &["common", "interface"], &["gfx"]]);
	}
	if has_extension(&["txt", "lua"]) {
		return inside_any(&[
			&["events"],
			&["decisions"],
			&["common", "scripted_effects"],
			&["common", "diplomatic_actions"],
			&["common", "triggered_modifiers"],
			&["common", "defines"],
			&["interface"],
			&["common", "interface"],
		]);
	}
	false
}

pub fn check_missing_dependency(ctx: &CheckContext) -> Vec<Finding> {
	let identity = crate::playset::dependency::ModIdentityIndex::from_mods(&ctx.mods);

	let mut findings = Vec::new();
	for mod_item in &ctx.mods {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			if identity.contains(dependency) {
				continue;
			}

			findings.push(new_finding(FindingArgs {
				rule_id: "missing-mod-dependency",
				severity: Severity::Warning,
				channel: FindingChannel::Advisory,
				message: "missing dependency".to_string(),
				mod_id: Some(mod_item.mod_id.clone()),
				path: None,
				source_file: mod_item.descriptor_path.clone(),
				evidence: Some(format!("{} depends on {dependency}", descriptor.name)),
				line: None,
				column: None,
				confidence: Some(0.9),
			}));
		}
	}

	findings
}

pub fn detect_dependency_misuse(ctx: &CheckContext) -> Vec<DepMisuseFinding> {
	let identity = crate::playset::dependency::ModIdentityIndex::from_mods(&ctx.mods);
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
			MergeKeySource::ContainerChildKey if is_container_child_scope(index, parent) => {
				insert_family_key(
					&mut keys_by_mod,
					&scope.mod_id,
					family_id,
					scope.key.clone(),
				);
			}
			MergeKeySource::ContainerChildFieldValue {
				containers,
				child_key_field,
				child_types,
			} => {
				if parent.kind == ScopeKind::File {
					if !containers.contains(&scope.key.as_str()) {
						insert_family_key(
							&mut keys_by_mod,
							&scope.mod_id,
							family_id,
							scope.key.clone(),
						);
					}
				} else if containers.contains(&parent.key.as_str())
					&& parent_parent_is_file(index, parent)
				{
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

fn dependency_merge_key_source_for_path(path: &GamePath) -> Option<(&'static str, MergeKeySource)> {
	let descriptor = eu4().classify_content_family(path)?;
	let source = descriptor.merge_key_source?;
	is_dependency_merge_key_source(source).then_some((descriptor.id.as_str(), source))
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

/// Whether a directory `mod_item` replaces holds a file `dep_mod` ships.
fn replace_path_covers_dependency_content(mod_item: &ModCandidate, dep_mod: &ModCandidate) -> bool {
	let Some(descriptor) = mod_item.descriptor.as_ref() else {
		return false;
	};
	dep_mod.files.iter().any(|file| {
		descriptor
			.replace_path
			.iter()
			.any(|prefix| file.starts_with(prefix))
	})
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
			new_finding(FindingArgs {
				rule_id: "mod-version-mismatch",
				severity: finding.severity,
				channel: FindingChannel::Advisory,
				message: finding.message.clone(),
				mod_id: Some(finding.mod_id.clone()),
				path: None,
				source_file: ctx
					.mods
					.iter()
					.find(|mod_item| mod_item.mod_id == finding.mod_id)
					.and_then(|mod_item| mod_item.descriptor_path.clone()),
				evidence: Some(format!(
					"tag={} supported_version={} game_version={}",
					finding.tag, finding.supported_version, finding.game_version
				)),
				line: None,
				column: None,
				confidence: Some(0.8),
			})
		})
		.collect()
}

pub fn check_dependency_misuse(ctx: &CheckContext) -> Vec<Finding> {
	detect_dependency_misuse(ctx)
		.into_iter()
		.map(|finding| {
			new_finding(FindingArgs {
				rule_id: "unused-mod-dependency",
				severity: Severity::Warning,
				channel: FindingChannel::Advisory,
				message: format!(
					"descriptor.mod dependencies misuse suspected: {} declares {} without semantic references",
					finding.mod_id, finding.suspicious_dep_id
				),
				mod_id: Some(finding.mod_id.clone()),
				path: None,
				source_file: ctx
					.mods
					.iter()
					.find(|mod_item| mod_item.mod_id == finding.mod_id)
					.and_then(|mod_item| mod_item.descriptor_path.clone()),
				evidence: Some(format!(
					"dep_id={} dep_display_name={} semantic_refs_to_dep={} false_remove_count={}",
					finding.suspicious_dep_id,
					finding.suspicious_dep_display_name,
					finding.evidence.semantic_refs_to_dep,
					finding.evidence.false_remove_count
				)),
				line: None,
				column: None,
				confidence: Some(0.8),
			})
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

fn display_name_for_mod(mod_item: &crate::model::ModCandidate) -> String {
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
			.map(|def| format!("{}:{}#L{}", def.mod_id, def.path, def.line))
			.collect::<Vec<_>>()
			.join("; ");
		let Some(last) = defs.last() else {
			continue;
		};
		findings.push(new_finding(FindingArgs {
			rule_id: "duplicate-scripted-effect",
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("duplicate scripted effect: {name}"),
			mod_id: Some(last.mod_id.clone()),
			path: Some(last.path.clone()),
			source_file: None,
			evidence: Some(evidence),
			line: Some(last.line),
			column: Some(last.column),
			confidence: Some(0.8),
		}));
	}

	findings
}

struct FindingArgs<'a> {
	rule_id: &'a str,
	severity: Severity,
	channel: FindingChannel,
	message: String,
	mod_id: Option<String>,
	path: Option<GamePathBuf>,
	source_file: Option<PathBuf>,
	evidence: Option<String>,
	line: Option<usize>,
	column: Option<usize>,
	confidence: Option<f32>,
}

fn new_finding(args: FindingArgs<'_>) -> Finding {
	let FindingArgs {
		rule_id,
		severity,
		channel,
		message,
		mod_id,
		path,
		source_file,
		evidence,
		line,
		column,
		confidence,
	} = args;
	Finding {
		rule_id: rule_id.to_string(),
		severity,
		channel,
		message,
		mod_id,
		path,
		source_file,
		evidence,
		line,
		column,
		confidence,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::script::{build_semantic_index, parse_script_file};
	use crate::model::{
		GamePathBuf, LocalisationDefinition, MaybeScope, ModCandidate, ScopeSet, SemanticIndex,
		SymbolDefinition, SymbolReference, test_support,
	};
	use crate::playset::descriptor::ModDescriptor;
	use crate::playset::{Playset, PlaysetEntry};
	use std::fs;
	use std::path::{Path, PathBuf};

	fn candidate(
		mod_id: &str,
		display_name: &str,
		descriptor_name: &str,
		deps: &[&str],
	) -> ModCandidate {
		ModCandidate {
			entry: PlaysetEntry {
				display_name: Some(display_name.to_string()),
				enabled: true,
				position: Some(0),
				steam_id: Some(mod_id.to_string()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: Some(PathBuf::from(format!("{mod_id}/descriptor.mod"))),
			descriptor: Some(ModDescriptor {
				name: descriptor_name.to_string(),
				dependencies: deps.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
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
		test_support::install_defaults();
		SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: format!("eu4::common.scripted_effects::{local_name}"),
			module: "common.scripted_effects".to_string(),
			local_name: local_name.to_string(),
			mod_id: mod_id.to_string(),
			path: crate::model::GamePathBuf::parse("common/scripted_effects/test.txt")
				.expect("valid game path"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: MaybeScope::Unknown,
			inferred_this_type: MaybeScope::Unknown,
			inferred_this_mask: ScopeSet::EMPTY,
			inferred_from_mask: ScopeSet::EMPTY,
			inferred_root_mask: ScopeSet::EMPTY,
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
			path: crate::model::GamePathBuf::parse("common/scripted_effects/caller.txt")
				.expect("valid game path"),
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
			playlist: Playset {
				mods: mods.iter().map(|mod_item| mod_item.entry.clone()).collect(),
				..Playset::default()
			},
			mods,
			semantic_index,
		}
	}

	fn with_files(mut mod_item: ModCandidate, root: PathBuf, files: &[&str]) -> ModCandidate {
		mod_item.root_path = Some(root);
		mod_item.files = files
			.iter()
			.map(|file| GamePathBuf::parse(file).expect("valid game path"))
			.collect();
		mod_item
	}

	fn write_fixture(root: &Path, relative: &str, contents: &str) {
		let path = root.join(relative);
		fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture dirs");
		fs::write(path, contents).expect("fixture file");
	}

	fn tempdir_in_target() -> tempfile::TempDir {
		let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/rules-tests");
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
			path: crate::model::GamePathBuf::parse("localisation/test_l_english.yml")
				.expect("valid game path"),
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
		let game_path = crate::model::GamePath::new(relative).expect("valid game path");
		let parsed = vec![
			parse_script_file("100", &main_root, game_path),
			parse_script_file("200", &dep_root, game_path),
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
		main.descriptor.as_mut().unwrap().replace_path =
			vec![GamePathBuf::parse("common/missions").expect("valid game path")];
		let mut dep = candidate("200", "Dependency Mod", "Dependency Mod", &[]);
		dep.files =
			vec![GamePathBuf::parse("common/missions/dep_missions.txt").expect("valid game path")];
		let ctx = context(vec![main, dep], SemanticIndex::default());

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	/// A `replace_path` covers the files in and below the directory it names,
	/// compared by whole components.
	#[test]
	fn replace_path_covers_dependency_files_in_and_below_its_directory_only() {
		let mut main = candidate("100", "Main Mod", "main", &["Dependency Mod"]);
		main.descriptor.as_mut().unwrap().replace_path =
			vec![GamePathBuf::parse("common/ideas").expect("valid game path")];
		for (file, covered) in [
			("common/ideas/x.txt", true),
			("common/ideas/nested/x.txt", true),
			("common/ideas", true),
			("common/ideas_extra/x.txt", false),
		] {
			let mut dep = candidate("200", "Dependency Mod", "Dependency Mod", &[]);
			dep.files = vec![GamePathBuf::parse(file).expect("valid game path")];
			assert_eq!(
				replace_path_covers_dependency_content(&main, &dep),
				covered,
				"{file}"
			);
		}
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

	/// The same verdicts the lowercased `starts_with`/`ends_with` text checks
	/// gave, now over whole components.
	#[test]
	fn structurally_mergeable_paths_match_whole_components_ignoring_ascii_case() {
		for (path, mergeable) in [
			("interface/x.gui", true),
			("Interface/X.GUI", true),
			("gfx/a.gfx", true),
			("common/interface/a.gfx", true),
			("common/Scripted_Effects/a.txt", true),
			("common/defines/x.lua", true),
			("events/sub/x.txt", true),
			("interface/x.txt", true),
			("common/scripted_effects_extra/a.txt", false),
			("events.txt", false),
			("interface.gui", false),
			("events/x.gui", false),
			("gfx/x.txt", false),
			("common/ideas/x.txt", false),
			("events/x.yml", false),
		] {
			assert_eq!(
				is_structurally_mergeable_path(GamePath::new(path).expect("valid game path")),
				mergeable,
				"{path}"
			);
		}
	}

	#[test]
	fn file_conflicts_come_out_in_game_path_byte_order_and_name_merge_candidates() {
		let files = [
			"events/b.txt",
			"common/a-b.txt",
			"common/a/b.txt",
			"events/a.txt",
		];
		let ctx = context(
			vec![
				with_files(candidate("100", "A", "a", &[]), PathBuf::from("a"), &files),
				with_files(candidate("200", "B", "b", &[]), PathBuf::from("b"), &files),
			],
			SemanticIndex::default(),
		);

		let findings = check_file_conflict(&ctx);

		assert_eq!(
			findings
				.iter()
				.map(|finding| finding.path.as_deref().map(GamePath::as_str))
				.collect::<Vec<_>>(),
			[
				Some("common/a-b.txt"),
				Some("common/a/b.txt"),
				Some("events/a.txt"),
				Some("events/b.txt"),
			]
		);
		assert_eq!(
			findings[2].message,
			"file overwrite conflict (structural auto-merge candidate): events/a.txt"
		);
		assert!(
			!findings[0].message.contains("auto-merge"),
			"{}",
			findings[0].message
		);
		assert!(findings.iter().all(|finding| finding.source_file.is_none()));
	}
}
