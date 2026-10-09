//! Compose culture identity changes before the structural merge consumes views.

use std::collections::{BTreeMap, BTreeSet};

use crate::game::eu4::content::eu4;
use crate::game::eu4::cultures::correspondence::{CultureCorrespondence, CultureGroupMove};
use crate::game::eu4::cultures::{CultureIndex, CultureRenameMap, transform_cultures};
use crate::game::eu4::script::documents::classify_document_family;
use crate::game::eu4::script::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use crate::game::eu4::script::parser::{AstFile, AstValue};
use crate::input::ResolvedInput;
use crate::model::{DocumentFamily, GamePathBuf};
use crate::project::DepOverride;

use crate::merge::dag::{IgnoreReplacePath, build_mod_dag};
use crate::merge::error::{MergeError, MergeErrorSubject};
use crate::merge::path_plan::build_merge_plan_from_input;
use crate::merge::planning::module_view::build_cross_file_module_views;

use crate::merge::transform::input::{
	ReviewedInputPolicy, ReviewedInputs, SourceBinding, SourceRepair,
};
use crate::merge::transform::{
	EmittedOutput, OutputValidation, PreparedTransform, ScriptTransform, TransformAdapter,
	TransformRequest,
};
use std::sync::Arc;

pub(crate) struct CultureAdapter;

impl TransformAdapter for CultureAdapter {
	fn infer_entity_transform(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
	) -> Result<Option<Arc<dyn crate::merge::transform::tree::EntityTransform>>, String> {
		CultureCorrespondence::from_files(base, revisions)
			.map(|transform| {
				transform.map(|value| {
					Arc::new(value) as Arc<dyn crate::merge::transform::tree::EntityTransform>
				})
			})
			.map_err(|findings| format!("culture correspondence needs review: {findings:?}"))
	}

	fn infer_dag_transform(
		&self,
		file_dag: &crate::merge::planning::dag::FileDag,
		vanilla: Option<&crate::game::eu4::script::ParsedScriptFile>,
		contributors: &std::collections::HashMap<
			crate::merge::planning::dag::ModId,
			crate::game::eu4::script::ParsedScriptFile,
		>,
		policies: &crate::game::eu4::content::MergePolicies,
	) -> Result<Option<Arc<dyn crate::merge::transform::tree::EntityTransform>>, String> {
		super::dag::culture_correspondence_for_dag(
			file_dag,
			vanilla,
			contributors,
			policies,
			&super::dag::ReviewedCultureMappings::default(),
		)
		.map(|transform| {
			transform.map(|value| {
				Arc::new(value) as Arc<dyn crate::merge::transform::tree::EntityTransform>
			})
		})
		.map_err(|findings| format!("culture correspondence needs review: {findings:?}"))
	}

	fn reviewed_inputs<'a>(
		&self,
		project: &'a crate::project::Project,
	) -> Result<ReviewedInputPolicy<'a>, MergeError> {
		let config = &project.cultures;
		config
			.validate()
			.map_err(|error| invalid(error.to_string()))?;
		let sources = config
			.renames
			.iter()
			.map(|entry| SourceBinding {
				mod_id: &entry.mod_id,
				file: &entry.file,
				sha256: &entry.sha256,
			})
			.collect::<Vec<_>>();
		let repairs = config
			.repairs
			.iter()
			.map(|entry| SourceRepair {
				source: SourceBinding {
					mod_id: &entry.mod_id,
					file: &entry.file,
					sha256: &entry.sha256,
				},
				edits: &entry.edits,
			})
			.collect::<Vec<_>>();
		Ok(ReviewedInputPolicy {
			label: "culture",
			sources,
			repairs,
			validate: Some(|path, ast| {
				CultureIndex::from_documents(&[(path.to_owned(), ast.clone())])
					.map(|_| ())
					.map_err(|findings| {
						format!("culture repair leaves an invalid hierarchy: {findings:?}")
					})
			}),
		})
	}

	fn prepare(
		&self,
		input: &mut ResolvedInput,
		request: &TransformRequest<'_>,
		reviewed: ReviewedInputs,
	) -> Result<Vec<PreparedTransform>, MergeError> {
		let config = &request.project.cultures;
		if !config.renames.is_empty() && input.installed_base_snapshot.is_none() {
			return Err(invalid(
				"reviewed culture mappings require an analyzed vanilla ancestor",
			));
		}
		let mut culture = prepare_cultures(
			input,
			request.dep_overrides,
			request.ignore_replace_path,
			request.emit_options,
			request.duplicate_definitions,
			&config.renames,
			request.reference_inventory_complete,
		)?;
		if !config.renames.is_empty()
			&& culture.correspondence.is_none()
			&& culture.findings.is_empty()
		{
			return Err(invalid(
				"cannot verify reviewed culture mappings against the analyzed vanilla and contributor culture views",
			));
		}
		let mut evidence = reviewed.evidence;
		for rename in &config.renames {
			evidence.push(format!(
				"reviewed culture mapping: {} -> {} from {}:{} SHA256 {}",
				rename.from, rename.to, rename.mod_id, rename.file, rename.sha256
			));
		}
		let identity = if config.is_empty() && culture.mapping.mappings().is_empty() {
			String::new()
		} else {
			blake3::hash(
				&serde_json::to_vec(&(culture.mapping.mappings(), config.identity()))
					.expect("culture decisions serialize"),
			)
			.to_hex()
			.to_string()
		};
		if !config.is_empty() {
			culture.paths.extend(
				config
					.repairs
					.iter()
					.filter_map(|entry| GamePathBuf::parse(&entry.file).ok()),
			);
			culture.paths.extend(
				config
					.renames
					.iter()
					.filter_map(|entry| GamePathBuf::parse(&entry.file).ok()),
			);
			evidence.push(format!("culture decisions identity: {}", config.identity()));
		}
		if !culture.mapping.mappings().is_empty() {
			evidence.push(format!(
				"culture identity adaptation: {}",
				culture
					.mapping
					.mappings()
					.iter()
					.map(|(old, new)| format!("{old} -> {new}"))
					.collect::<Vec<_>>()
					.join(", ")
			));
		}
		Ok(vec![PreparedTransform {
			id: "eu4.culture".into(),
			identity,
			paths: std::mem::take(&mut culture.paths),
			findings: std::mem::take(&mut culture.findings),
			evidence,
			entity_transform: culture.correspondence.take().map(|value| {
				Arc::new(value) as Arc<dyn crate::merge::transform::tree::EntityTransform>
			}),
			source_guard: reviewed.source_guard,
			overlays: std::mem::take(&mut culture.overlays),
			generated: Vec::new(),
			validator: Some(Arc::new(culture)),
		}])
	}
}

#[derive(Clone, Debug, Default)]
struct CultureAdaptations {
	mapping: CultureRenameMap,
	correspondence: Option<CultureCorrespondence>,
	paths: BTreeSet<GamePathBuf>,
	findings: Vec<String>,
	overlays: Vec<ScriptTransform>,
}

impl OutputValidation for CultureAdaptations {
	fn validate_emitted(&self, output: &EmittedOutput) -> Vec<String> {
		let documents = output.scripts.as_slice();
		if self.mapping.mappings().is_empty() {
			return Vec::new();
		}
		let catalog = match CultureIndex::from_documents(documents) {
			Ok(catalog) => catalog,
			Err(findings) => {
				return findings
					.into_iter()
					.map(|finding| finding.message)
					.collect();
			}
		};
		let mut findings = Vec::new();
		for (old, new) in self.mapping.mappings() {
			if catalog.definitions.contains_key(old) || !catalog.definitions.contains_key(new) {
				findings.push(format!(
					"emitted culture definitions do not implement `{old}` -> `{new}`"
				));
			}
		}
		for (path, ast) in documents {
			let audited = transform_cultures(ast, path, &self.mapping);
			findings.extend(
				audited
					.findings
					.into_iter()
					.map(|finding| format!("{path}: {}", finding.message)),
			);
			for change in audited
				.changes
				.into_iter()
				.filter(|change| !change.definition)
			{
				findings.push(format!(
					"{}:{}:{} still references renamed culture `{}`",
					path,
					change.location.span.start.line,
					change.location.span.start.column,
					change.from
				));
			}
		}
		findings
	}
}

fn prepare_cultures(
	input: &mut ResolvedInput,
	dep_overrides: &[DepOverride],
	ignore_replace_path: &IgnoreReplacePath,
	emit_options: &EmitOptions,
	duplicate_definitions: Option<crate::game::eu4::content::DuplicateDefinitionPolicy>,
	reviewed_entries: &[crate::project::CultureRenameEntry],
	reference_inventory_complete: bool,
) -> Result<CultureAdaptations, MergeError> {
	if input.installed_base_snapshot.is_none() {
		return Ok(CultureAdaptations::default());
	}
	let plan = build_merge_plan_from_input(input, true);
	let Some((entry, namespace)) = plan.paths.iter().find_map(|entry| {
		entry
			.target
			.module_outputs()
			.iter()
			.find(|namespace| namespace.namespace_prefix().as_str() == "common/cultures")
			.map(|namespace| (entry, namespace))
	}) else {
		return Ok(CultureAdaptations::default());
	};
	let descriptor = eu4()
		.classify_content_family(namespace.output_path())
		.expect("planned culture family");
	let (mod_dag, _) = build_mod_dag(&input.mods);
	let Ok(views) = build_cross_file_module_views(
		entry,
		namespace,
		input,
		descriptor,
		&mod_dag,
		ignore_replace_path,
		dep_overrides,
		duplicate_definitions,
	) else {
		// Existing module analysis reports malformed contributors with their source.
		return Ok(CultureAdaptations::default());
	};
	let Some(vanilla) = &views.vanilla else {
		return Ok(CultureAdaptations::default());
	};
	let document_path =
		GamePathBuf::parse("common/cultures/merged.txt").expect("culture document path");
	let Ok(before) = CultureIndex::from_documents(&[(document_path.clone(), vanilla.ast.clone())])
	else {
		return Ok(CultureAdaptations::default());
	};
	let mut revisions = Vec::new();
	for mod_id in mod_dag.topo() {
		let Some(view) = views.contributors.get(mod_id) else {
			continue;
		};
		match CultureIndex::from_documents(&[(document_path.clone(), view.ast.clone())]) {
			Ok(index) => {
				revisions.push(index);
			}
			Err(_) => return Ok(CultureAdaptations::default()),
		}
	}
	let reviewed =
		super::dag::ReviewedCultureMappings::from_verified_entries(reviewed_entries, &views);
	let correspondence = match reviewed.and_then(|reviewed| {
		super::dag::culture_correspondence_for_dag(
			&views.file_dag,
			views.vanilla.as_ref(),
			&views.contributors,
			&descriptor.merge_policies,
			&reviewed,
		)
	}) {
		Ok(Some(correspondence)) => correspondence,
		Ok(None) => return Ok(CultureAdaptations::default()),
		Err(findings) => {
			let identities = std::iter::once(&before)
				.chain(&revisions)
				.flat_map(|index| index.definitions.keys().map(|id| (id.clone(), id.clone())))
				.collect();
			let mut paths = indexed_reference_paths(input, &identities)
				.into_iter()
				.map(|(_, path)| path)
				.collect::<BTreeSet<_>>();
			paths.extend(entry.target.input_paths().iter().cloned());
			return Ok(CultureAdaptations {
				mapping: CultureRenameMap::default(),
				paths,
				findings: findings
					.into_iter()
					.map(|finding| finding.message)
					.collect(),
				..Default::default()
			});
		}
	};
	let mappings = correspondence.renames().clone();
	let group_review = review_group_moves(
		input,
		&before,
		&revisions,
		&correspondence.group_moves,
		reference_inventory_complete,
	);
	if mappings.is_empty() && group_review.findings.is_empty() {
		return Ok(CultureAdaptations {
			correspondence: Some(correspondence),
			..Default::default()
		});
	}
	let mut mapping_before = before.clone();
	for old in mappings
		.keys()
		.filter(|old| !before.definitions.contains_key(*old))
	{
		if let Some(definition) = revisions
			.iter()
			.find_map(|index| index.definitions.get(old))
		{
			mapping_before
				.definitions
				.insert(old.clone(), definition.clone());
		}
	}
	let mut after = mapping_before.clone();
	for (old, new) in &mappings {
		after.definitions.remove(old);
		after.definitions.insert(
			new.clone(),
			revisions
				.iter()
				.find_map(|index| index.definitions.get(new))
				.expect("verified rename target")
				.clone(),
		);
	}
	let mapping = match CultureRenameMap::checked(&mapping_before, &after, &mappings) {
		Ok(mapping) => mapping,
		Err(findings) => {
			let mut paths = indexed_reference_paths(input, &mappings)
				.into_iter()
				.map(|(_, path)| path)
				.collect::<BTreeSet<_>>();
			paths.extend(entry.target.input_paths().iter().cloned());
			return Ok(CultureAdaptations {
				mapping: CultureRenameMap::default(),
				paths,
				findings: findings
					.into_iter()
					.map(|finding| finding.message)
					.collect(),
				..Default::default()
			});
		}
	};
	let mut affected_identities = mappings.clone();
	for id in group_review.identities {
		affected_identities.entry(id.clone()).or_insert(id);
	}
	let reference_paths = indexed_reference_paths(input, &affected_identities);
	let mut adapted = Vec::new();
	let mut paths = group_review.paths;
	let mut findings = group_review.findings;
	let parameter_review =
		super::parameters::review_parameter_flows(input, &affected_identities).map_err(invalid)?;
	paths.extend(parameter_review.paths);
	findings.extend(parameter_review.findings);
	if !findings.is_empty() {
		paths.extend(reference_paths.iter().map(|(_, path)| path.clone()));
	}
	if input.requested_retained_paths.is_some() {
		findings.push("culture adaptation needs the complete playset reference index; a retained-path analysis cannot prove reference closure".to_owned());
	}
	if !reference_inventory_complete {
		findings.push("culture adaptation cannot prove reference closure with extra_ignore_patterns; analyze without user file filters before accepting the transformation".to_owned());
	}
	paths.extend(entry.target.input_paths().iter().cloned());
	for (path, contributors) in &input.file_inventory {
		if path.is_inside(&["common", "cultures"], str::eq)
			|| classify_document_family(path) != Some(DocumentFamily::Clausewitz)
		{
			continue;
		}
		for contributor in contributors.iter().filter(|item| !item.is_synthetic_base) {
			if !reference_paths.contains(&(contributor.mod_id.clone(), path.clone())) {
				continue;
			}
			let parsed = input.script_cache.load(contributor).map_err(invalid)?;
			let result = transform_cultures(&parsed.ast, path, &mapping);
			if result
				.references
				.iter()
				.any(|reference| mappings.values().any(|target| target == &reference.id))
			{
				paths.insert(path.clone());
			}
			if !result.findings.is_empty() {
				paths.insert(path.clone());
				findings.extend(result.findings.iter().map(|finding| {
					let position = finding
						.location
						.as_ref()
						.map(|location| {
							format!(
								":{}:{}",
								location.span.start.line, location.span.start.column
							)
						})
						.unwrap_or_default();
					format!(
						"{}:{path}{position}: {}",
						contributor.mod_id, finding.message
					)
				}));
				continue;
			}
			if result.changes.is_empty() {
				continue;
			}
			if !parsed.parse_issues.is_empty() {
				paths.insert(path.clone());
				findings.push(format!(
					"culture adaptation requires a repaired input: {}:{path}",
					contributor.mod_id
				));
				continue;
			}
			let rendered =
				emit_clausewitz_statements_with_options(&result.ast.statements, emit_options)
					.map_err(|error| invalid(error.to_string()))?;
			adapted.push(ScriptTransform::new(
				parsed.clone(),
				result.ast,
				rendered.into_bytes(),
			));
			paths.insert(path.clone());
		}
	}
	Ok(CultureAdaptations {
		mapping,
		correspondence: Some(correspondence),
		paths,
		findings,
		overlays: adapted,
	})
}

#[derive(Default)]
struct GroupMoveReview {
	identities: BTreeSet<String>,
	paths: BTreeSet<GamePathBuf>,
	findings: Vec<String>,
}

fn review_group_moves(
	input: &ResolvedInput,
	before: &CultureIndex,
	revisions: &[CultureIndex],
	moves: &[CultureGroupMove],
	reference_inventory_complete: bool,
) -> GroupMoveReview {
	let mut review = GroupMoveReview::default();
	for CultureGroupMove {
		old,
		new,
		from,
		to,
		inherited_fields,
	} in moves
	{
		let original = group_metadata(inherited_fields);
		let inherited_change = revisions.iter().any(|index| {
			(index.groups.contains_key(to)
				&& group_metadata(
					index
						.group_fields
						.get(to)
						.map(Vec::as_slice)
						.unwrap_or_default(),
				) != original)
				|| (index.groups.contains_key(from)
					&& group_metadata(
						index
							.group_fields
							.get(from)
							.map(Vec::as_slice)
							.unwrap_or_default(),
					) != original)
		});
		let mut group_uses = BTreeSet::new();
		let known_group = |value: &str| {
			before.groups.contains_key(value)
				|| revisions
					.iter()
					.any(|index| index.groups.contains_key(value))
		};
		let dynamic_group_use = |key: &str, value: &str| {
			key.contains("culture_group") && (!known_group(value) || value == from || value == to)
		};
		for snapshot in input.mod_snapshots.iter().flatten() {
			for reference in &snapshot.semantic_index.resource_references {
				if reference.key == "culture_group_reference"
					&& (&reference.value == from || &reference.value == to)
				{
					group_uses.insert(reference.path.clone());
				}
			}
			for assignment in &snapshot.semantic_index.scalar_assignments {
				if dynamic_group_use(&assignment.key, &assignment.value) {
					group_uses.insert(assignment.path.clone());
				}
			}
		}
		if let Some(base) = &input.installed_base_snapshot {
			for reference in &base.snapshot.resource_references {
				if reference.key == "culture_group_reference"
					&& (&reference.value == from || &reference.value == to)
				{
					group_uses.insert(reference.path.clone());
				}
			}
			for assignment in &base.snapshot.scalar_assignments {
				if dynamic_group_use(&assignment.key, &assignment.value) {
					group_uses.insert(assignment.path.clone());
				}
			}
		}
		if inherited_change
			|| !group_uses.is_empty()
			|| input.requested_retained_paths.is_some()
			|| !reference_inventory_complete
		{
			review.findings.push(format!("culture `{old}` -> `{new}` moves from group `{from}` to `{to}`; review changed inherited fields or culture-group predicates ({})", group_uses.iter().map(|path| path.as_str()).collect::<Vec<_>>().join(", ")));
			review.identities.extend([old.clone(), new.clone()]);
			review.paths.extend(group_uses);
		}
	}
	review
}

fn group_metadata(fields: &[(String, AstValue)]) -> Vec<(String, String)> {
	fields
		.iter()
		.map(|(key, value)| {
			(
				key.clone(),
				crate::merge::semantic_fingerprint::value_fingerprint(value),
			)
		})
		.collect()
}

/// Use the shared vanilla/mod snapshot indexes to select the documents to adapt.
/// Scalar occurrences also enter the audit so unsupported uses of a changed ID
/// cannot disappear simply because they lack a typed reference binding.
fn indexed_reference_paths(
	input: &ResolvedInput,
	mappings: &BTreeMap<String, String>,
) -> BTreeSet<(String, GamePathBuf)> {
	let identities = mappings
		.keys()
		.chain(mappings.values())
		.collect::<BTreeSet<_>>();
	let mut paths = BTreeSet::new();
	for snapshot in input.mod_snapshots.iter().flatten() {
		for reference in &snapshot.semantic_index.resource_references {
			if reference.key == "culture_reference" && identities.contains(&reference.value) {
				paths.insert((reference.mod_id.clone(), reference.path.clone()));
			}
		}
		for assignment in &snapshot.semantic_index.scalar_assignments {
			if assignment.key.contains("culture") && mappings.contains_key(&assignment.value) {
				paths.insert((assignment.mod_id.clone(), assignment.path.clone()));
			}
		}
	}
	if let Some(installed) = &input.installed_base_snapshot {
		let base_paths = installed
			.snapshot
			.resource_references
			.iter()
			.filter(|reference| {
				reference.key == "culture_reference" && identities.contains(&reference.value)
			})
			.map(|reference| reference.path.clone())
			.chain(
				installed
					.snapshot
					.scalar_assignments
					.iter()
					.filter(|assignment| {
						assignment.key.contains("culture")
							&& mappings.contains_key(&assignment.value)
					})
					.map(|assignment| assignment.path.clone()),
			)
			.collect::<BTreeSet<_>>();
		for path in base_paths {
			if let Some(contributors) = input.file_inventory.get(&path) {
				for contributor in contributors
					.iter()
					.filter(|contributor| contributor.is_base_game)
				{
					paths.insert((contributor.mod_id.clone(), path.clone()));
				}
			}
		}
	}
	paths
}

fn invalid(message: impl Into<String>) -> MergeError {
	MergeError::Validation {
		subject: Some(MergeErrorSubject::Named("common/cultures".to_owned())),
		message: message.into(),
	}
}

#[cfg(test)]
mod tests {
	use crate::merge::transform::input::repair_text;
	use crate::project::SourceEdit;

	fn game_path(text: &str) -> crate::model::GamePathBuf {
		crate::model::GamePathBuf::parse(text).expect("test game path")
	}
	fn edit(source: &str, expected: &str, replacement: &str) -> SourceEdit {
		let start = source.find(expected).unwrap();
		SourceEdit {
			start,
			end: start + expected.len(),
			expected: expected.into(),
			replacement: replacement.into(),
		}
	}

	#[test]
	fn exact_edits_fix_missing_openers_without_changing_the_input() {
		let source =
			include_str!("../../../../tests/fixtures/cultures/malformed/missing_group_openers.txt");
		let edits = [
			"thai_group",
			"eastern_algonquian",
			"plains_algonquian",
			"sonoran",
		]
		.map(|group| edit(source, group, &format!("{group} = {{")));
		let repaired = repair_text(source, &edits).unwrap();
		let path = game_path("common/cultures/test.txt");
		let parsed = parse_clausewitz_content(&path, &repaired);
		assert!(parsed.diagnostics.is_empty());
		assert_eq!(
			CultureIndex::from_documents(&[(path, parsed.ast)])
				.unwrap()
				.definitions
				.len(),
			4
		);
		assert!(!source.contains("thai_group = {"));
	}

	#[test]
	fn extra_brace_repair_reparses_but_balanced_wrong_hierarchy_is_rejected() {
		let path = game_path("common/cultures/test.txt");
		let source =
			include_str!("../../../../tests/fixtures/cultures/malformed/extra_closing_brace.txt");
		let parsed = parse_clausewitz_content(&path, source);
		assert_eq!(parsed.diagnostics.len(), 1);
		let start = parsed.diagnostics[0].span.start.offset;
		let repaired = repair_text(
			source,
			&[SourceEdit {
				start,
				end: start + 1,
				expected: "}".into(),
				replacement: "".into(),
			}],
		)
		.unwrap();
		let parsed = parse_clausewitz_content(&path, &repaired);
		assert!(parsed.diagnostics.is_empty());
		assert_eq!(
			CultureIndex::from_documents(&[(path.clone(), parsed.ast)])
				.unwrap()
				.groups
				.len(),
			2
		);
		let source =
			include_str!("../../../../tests/fixtures/cultures/malformed/premature_group_close.txt");
		let parsed = parse_clausewitz_content(&path, source);
		assert!(parsed.diagnostics.is_empty());
		assert!(CultureIndex::from_documents(&[(path, parsed.ast)]).is_err());
	}
	use super::*;
	use crate::game::eu4::script::parser::parse_clausewitz_content;

	#[test]
	fn final_adaptation_validation_checks_actual_definitions_and_references() {
		let adaptations = CultureAdaptations {
			mapping: CultureRenameMap::checked(
				&CultureIndex::from_documents(&[(
					game_path("common/cultures/base.txt"),
					parse_clausewitz_content(
						&game_path("common/cultures/base.txt"),
						"g = { old = { primary = AAA } }",
					)
					.ast,
				)])
				.unwrap(),
				&CultureIndex::from_documents(&[(
					game_path("common/cultures/base.txt"),
					parse_clausewitz_content(
						&game_path("common/cultures/base.txt"),
						"g = { renamed = { primary = AAA } }",
					)
					.ast,
				)])
				.unwrap(),
				&BTreeMap::from([("old".into(), "renamed".into())]),
			)
			.unwrap(),
			..Default::default()
		};
		let document = |path: &str, source: &str| {
			let path = game_path(path);
			let parsed = parse_clausewitz_content(&path, source);
			assert!(parsed.diagnostics.is_empty());
			(path, parsed.ast)
		};
		let culture = document(
			"common/cultures/merged.txt",
			"g = { renamed = { primary = AAA } }",
		);
		let effect = document(
			"common/scripted_effects/merged.txt",
			"effect = { change_culture = renamed set_country_flag = old }",
		);
		assert!(
			adaptations
				.validate_emitted(&EmittedOutput {
					scripts: vec![culture.clone(), effect.clone()],
					..Default::default()
				})
				.is_empty()
		);
		let stale_effect = document(
			"common/scripted_effects/merged.txt",
			"effect = { change_culture = old }",
		);
		assert!(
			!adaptations
				.validate_emitted(&EmittedOutput {
					scripts: vec![culture.clone(), stale_effect],
					..Default::default()
				})
				.is_empty()
		);
		let unsupported = document(
			"common/scripted_effects/merged.txt",
			"effect = { unknown_culture_operation = old }",
		);
		assert!(
			!adaptations
				.validate_emitted(&EmittedOutput {
					scripts: vec![culture, unsupported],
					..Default::default()
				})
				.is_empty()
		);
		let stale_culture = document(
			"common/cultures/merged.txt",
			"g = { old = { primary = AAA } }",
		);
		assert!(
			!adaptations
				.validate_emitted(&EmittedOutput {
					scripts: vec![stale_culture, effect],
					..Default::default()
				})
				.is_empty()
		);
	}
}
