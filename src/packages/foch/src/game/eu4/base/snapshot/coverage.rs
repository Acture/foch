use super::{
	BaseAliasUsage, BaseAnalysisSnapshot, BaseCsvRow, BaseJsonProperty, BaseKeyUsage,
	BaseLocalisationDefinition, BaseLocalisationDuplicate, BaseResourceReference,
	BaseScalarAssignment, BaseSymbolDefinition, BaseSymbolReference, BaseUiDefinition,
};
use crate::game::eu4::content::eu4_content_family_for_root_family;
use crate::game::eu4::script::classify_script_file;
use crate::model::{DocumentFamily, GamePath};
use relative_path::RelativePath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageClass {
	ExcludedNonGameplay,
	ParseOnly,
	SemanticComplete,
	GraphReady,
	MergeReady,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RootCoverageEntry {
	pub root_family: String,
	pub coverage_class: CoverageClass,
	pub inventory_file_count: usize,
	pub document_count: usize,
	pub parse_failed_documents: usize,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub document_families: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub script_file_kinds: Vec<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub semantic_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BaseCoverageReport {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub generated_by_cli_version: String,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub class_counts: BTreeMap<String, usize>,
	pub roots: Vec<RootCoverageEntry>,
}

#[derive(Default)]
struct CoverageAccumulator {
	inventory_file_count: usize,
	document_count: usize,
	parse_failed_documents: usize,
	document_families: BTreeMap<String, usize>,
	script_file_kinds: BTreeMap<String, usize>,
	semantic_counts: BTreeMap<String, usize>,
}

impl CoverageAccumulator {
	fn increment_document_family(&mut self, family: DocumentFamily) {
		let key = document_family_name(family).to_string();
		*self.document_families.entry(key).or_insert(0) += 1;
	}

	fn increment_script_file_kind(&mut self, kind: &str) {
		*self.script_file_kinds.entry(kind.to_string()).or_insert(0) += 1;
	}

	fn increment_semantic_count(&mut self, key: &str) {
		*self.semantic_counts.entry(key.to_string()).or_insert(0) += 1;
	}

	fn has_only_other_clausewitz(&self) -> bool {
		!self.script_file_kinds.is_empty()
			&& self
				.script_file_kinds
				.keys()
				.all(|item| item.as_str() == "other")
	}

	fn has_graph_semantics(&self) -> bool {
		self.semantic_counts.contains_key("symbol_definitions")
			|| self.semantic_counts.contains_key("symbol_references")
			|| self.semantic_counts.contains_key("alias_usages")
			|| self.semantic_counts.contains_key("key_usages")
			|| self.semantic_counts.contains_key("scalar_assignments")
	}

	fn has_semantic_surface(&self) -> bool {
		self.has_graph_semantics()
			|| self
				.semantic_counts
				.contains_key("localisation_definitions")
			|| self.semantic_counts.contains_key("localisation_duplicates")
			|| self.semantic_counts.contains_key("ui_definitions")
			|| self.semantic_counts.contains_key("resource_references")
			|| self.semantic_counts.contains_key("csv_rows")
			|| self.semantic_counts.contains_key("json_properties")
			|| self
				.script_file_kinds
				.keys()
				.any(|item| item.as_str() != "other")
	}
}

pub fn build_coverage_report(snapshot: &BaseAnalysisSnapshot) -> BaseCoverageReport {
	let mut roots: BTreeMap<String, CoverageAccumulator> = BTreeMap::new();
	for path in &snapshot.inventory_paths {
		let Some(root_family) = coverage_root_family(path) else {
			continue;
		};
		roots.entry(root_family).or_default().inventory_file_count += 1;
	}
	for document in &snapshot.documents {
		let Some(root_family) = coverage_root_family(&document.path) else {
			continue;
		};
		let entry = roots.entry(root_family).or_default();
		entry.document_count += 1;
		if !document.parse_ok {
			entry.parse_failed_documents += 1;
		}
		entry.increment_document_family(document.family);
		if document.family == DocumentFamily::Clausewitz {
			let kind = classify_script_file(&document.path);
			entry.increment_script_file_kind(kind.as_str());
		}
	}
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.symbol_definitions,
		"symbol_definitions",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.symbol_references, "symbol_references");
	accumulate_semantic_counts(&mut roots, &snapshot.alias_usages, "alias_usages");
	accumulate_semantic_counts(&mut roots, &snapshot.key_usages, "key_usages");
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.scalar_assignments,
		"scalar_assignments",
	);
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.localisation_definitions,
		"localisation_definitions",
	);
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.localisation_duplicates,
		"localisation_duplicates",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.ui_definitions, "ui_definitions");
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.resource_references,
		"resource_references",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.csv_rows, "csv_rows");
	accumulate_semantic_counts(&mut roots, &snapshot.json_properties, "json_properties");

	let mut class_counts: BTreeMap<String, usize> = BTreeMap::new();
	let entries: Vec<RootCoverageEntry> = roots
		.into_iter()
		.map(|(root_family, accumulator)| {
			let coverage_class = coverage_class_for_root(&root_family, &accumulator);
			*class_counts
				.entry(coverage_class_name(coverage_class).to_string())
				.or_insert(0) += 1;
			RootCoverageEntry {
				root_family,
				coverage_class,
				inventory_file_count: accumulator.inventory_file_count,
				document_count: accumulator.document_count,
				parse_failed_documents: accumulator.parse_failed_documents,
				document_families: accumulator.document_families.into_keys().collect(),
				script_file_kinds: accumulator.script_file_kinds.into_keys().collect(),
				semantic_counts: accumulator.semantic_counts,
			}
		})
		.collect();

	BaseCoverageReport {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		class_counts,
		roots: entries,
	}
}

fn coverage_class_for_root(root_family: &str, accumulator: &CoverageAccumulator) -> CoverageClass {
	if is_excluded_non_gameplay_root(root_family) {
		CoverageClass::ExcludedNonGameplay
	} else if let Some(classification) = content_family_coverage_class(root_family, accumulator) {
		classification
	} else if accumulator.has_only_other_clausewitz() {
		CoverageClass::ParseOnly
	} else if accumulator.has_graph_semantics() {
		CoverageClass::GraphReady
	} else if accumulator.has_semantic_surface() {
		CoverageClass::SemanticComplete
	} else {
		CoverageClass::ParseOnly
	}
}

fn content_family_coverage_class(
	root_family: &str,
	accumulator: &CoverageAccumulator,
) -> Option<CoverageClass> {
	let descriptor = eu4_content_family_for_root_family(root_family)?;
	if descriptor.capabilities.semantic_complete {
		return Some(CoverageClass::SemanticComplete);
	}
	if descriptor.capabilities.merge_ready {
		return Some(CoverageClass::MergeReady);
	}
	if descriptor.capabilities.graph_ready {
		return Some(if accumulator.has_graph_semantics() {
			CoverageClass::GraphReady
		} else {
			CoverageClass::ParseOnly
		});
	}
	Some(CoverageClass::ParseOnly)
}

pub fn coverage_class_name(classification: CoverageClass) -> &'static str {
	match classification {
		CoverageClass::ExcludedNonGameplay => "excluded_non_gameplay",
		CoverageClass::ParseOnly => "parse_only",
		CoverageClass::SemanticComplete => "semantic_complete",
		CoverageClass::GraphReady => "graph_ready",
		CoverageClass::MergeReady => "merge_ready",
	}
}

fn accumulate_semantic_counts<T>(
	roots: &mut BTreeMap<String, CoverageAccumulator>,
	items: &[T],
	metric_name: &str,
) where
	T: CoveragePath,
{
	for item in items {
		let Some(root_family) = coverage_root_family(item.coverage_path()) else {
			continue;
		};
		roots
			.entry(root_family)
			.or_default()
			.increment_semantic_count(metric_name);
	}
}

trait CoveragePath {
	fn coverage_path(&self) -> &GamePath;
}

impl CoveragePath for BaseSymbolDefinition {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseSymbolReference {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseAliasUsage {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseKeyUsage {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseScalarAssignment {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseLocalisationDefinition {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseLocalisationDuplicate {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseUiDefinition {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseResourceReference {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseCsvRow {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

impl CoveragePath for BaseJsonProperty {
	fn coverage_path(&self) -> &GamePath {
		&self.path
	}
}

/// The coverage root a game path counts toward: its content family's root
/// directory, spelled as the family id is.
fn coverage_root_family(path: &GamePath) -> Option<String> {
	if !is_tracked_non_binary_path(path) {
		return None;
	}
	let parts: Vec<&str> = path.iter().collect();
	match parts.as_slice() {
		["events", "common", group, ..] => {
			return Some(format!("events/common/{}", component_stem(group)));
		}
		["events", "decisions", ..] => return Some("events/decisions".to_string()),
		["map", "random", "tiles", _, ..] => return Some("map/random/tiles".to_string()),
		[
			"map",
			"random",
			"RandomLandNames.txt" | "RandomSeaNames.txt" | "RandomLakeNames.txt",
		] => {
			return Some("map/random_names".to_string());
		}
		["map", "random", "RNWScenarios.txt"] => return Some("map/random/scenarios".to_string()),
		_ => {}
	}
	match parts.as_slice() {
		[] | ["common" | "history" | "map"] => None,
		[top @ ("common" | "history" | "map"), file] => {
			Some(format!("{top}/{}", component_stem(file)))
		}
		[top @ ("common" | "history" | "map"), group, ..] => Some(format!("{top}/{group}")),
		[top, ..] => Some((*top).to_string()),
	}
}

/// A path component without its extension.
fn component_stem(name: &str) -> &str {
	RelativePath::new(name).file_stem().unwrap_or(name)
}

/// Text files the coverage report tracks, by extension in any case.
fn is_tracked_non_binary_path(path: &GamePath) -> bool {
	path.extension().is_some_and(|extension| {
		[
			"txt", "gui", "gfx", "asset", "mod", "lua", "yml", "yaml", "csv", "json",
		]
		.iter()
		.any(|tracked| extension.eq_ignore_ascii_case(tracked))
	})
}

fn is_excluded_non_gameplay_root(root_family: &str) -> bool {
	matches!(
		root_family,
		"licenses"
			| "patchnotes"
			| "ebook" | "legal_notes"
			| "builtin_dlc"
			| "dlc_metadata"
			| "hints" | "tools"
			| "tests" | "ThirdPartyLicenses.txt"
			| "map/random"
			| "checksum_manifest.txt"
			| "clausewitz_branch.txt"
			| "clausewitz_rev.txt"
			| "eu4_branch.txt"
			| "eu4_rev.txt"
			| "steam.txt"
			| "描述.txt"
			| "launcher-settings.json"
			| "settings-layout.json"
	)
}

pub fn document_family_name(family: DocumentFamily) -> &'static str {
	match family {
		DocumentFamily::Clausewitz => "clausewitz",
		DocumentFamily::Localisation => "localisation",
		DocumentFamily::Csv => "csv",
		DocumentFamily::Json => "json",
	}
}

pub fn write_coverage_report(path: &Path, snapshot: &BaseAnalysisSnapshot) -> Result<(), String> {
	let coverage = build_coverage_report(snapshot);
	let raw = serde_json::to_string_pretty(&coverage)
		.map_err(|err| format!("failed to serialize base data coverage: {err}"))?;
	fs::write(path, raw).map_err(|err| {
		format!(
			"failed to write base data coverage {}: {err}",
			path.display()
		)
	})
}

#[cfg(test)]
mod tests {
	use super::coverage_root_family;
	use crate::model::GamePath;

	#[test]
	fn coverage_roots_are_derived_from_game_path_components() {
		for (path, root) in [
			(
				"events/common/on_actions/x.txt",
				Some("events/common/on_actions"),
			),
			("events/common/foo.txt", Some("events/common/foo")),
			("events/decisions/x.txt", Some("events/decisions")),
			("events/x.txt", Some("events")),
			("map/random/tiles/tile0.txt", Some("map/random/tiles")),
			("map/random/RandomLakeNames.txt", Some("map/random_names")),
			("map/random/RNWScenarios.txt", Some("map/random/scenarios")),
			("map/random/other.txt", Some("map/random")),
			("map/area.txt", Some("map/area")),
			("common/ideas/00_x.txt", Some("common/ideas")),
			("common/achievements.txt", Some("common/achievements")),
			("common/defines.lua", Some("common/defines")),
			("common/Ideas/x.txt", Some("common/Ideas")),
			(
				"history/countries/SWE - Sweden.txt",
				Some("history/countries"),
			),
			("localisation/x_l_english.YML", Some("localisation")),
			("ThirdPartyLicenses.txt", Some("ThirdPartyLicenses.txt")),
			("gfx/x.dds", None),
			("common/no_extension", None),
		] {
			assert_eq!(
				coverage_root_family(GamePath::new(path).expect("valid game path")).as_deref(),
				root,
				"{path}"
			);
		}
	}
}
