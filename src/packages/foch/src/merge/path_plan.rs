use super::error::{MergeError, MergeErrorSubject};
use super::normalize::normalize_defines_file;
use crate::game::eu4::Eu4;
use crate::game::eu4::content::eu4;
use crate::game::eu4::content::load_rules::{DatabaseLoadRules, load_rules_for_version};
use crate::game::eu4::content::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
};
use crate::game::eu4::script::documents::{classify_document_family, is_clausewitz_defines_path};
use crate::input::{
	InputResolveError, InputResolveErrorKind, InputScriptCache, ResolvedInput,
	ResolvedInputContributor,
};
use crate::model::{
	DocumentFamily, GamePath, MergeModuleOutput, MergeModuleOutputs, MergePlanContributor,
	MergePlanEntry, MergePlanResult, MergePlanStrategies, MergePlanStrategy, MergePlanTarget,
	MergeUnitId,
};
use crate::playset::descriptor::slash_path_text;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn fatal_plan_from_input_error(
	err: &InputResolveError,
	include_game_base: bool,
) -> MergePlanResult {
	let mut result = MergePlanResult {
		generated_at: current_generated_at(),
		include_game_base,
		..MergePlanResult::default()
	};
	if err.kind == InputResolveErrorKind::PlaylistFormat {
		result.push_fatal_error("failed to parse Playset JSON");
	} else {
		result.push_fatal_error(err.message.clone());
	}
	result
}

pub(crate) fn build_merge_plan_from_input(
	input: &ResolvedInput,
	include_game_base: bool,
) -> MergePlanResult {
	let mut result = MergePlanResult {
		generated_at: current_generated_at(),
		include_game_base,
		..MergePlanResult::default()
	};

	result.game = input.playlist.game.key().to_string();
	result.playset_name = input.playlist.name.clone();

	let profile = eu4();
	match build_merge_units(input, profile) {
		Ok(paths) => match validate_structural_snapshot(input, profile, &paths) {
			Ok(()) => result.paths = paths,
			Err(error) => result.push_fatal_error(error),
		},
		Err(error) => result.push_fatal_error(error),
	}
	result.strategies = summarize_paths(&result.paths);
	result
}

type ModuleInputs<'a> = Vec<(&'a GamePath, &'a [ResolvedInputContributor])>;

fn build_merge_units(input: &ResolvedInput, profile: &Eu4) -> Result<Vec<MergePlanEntry>, String> {
	let rules: Option<&DatabaseLoadRules> = input
		.game_version
		.as_deref()
		.and_then(load_rules_for_version);
	let mut databases: BTreeMap<&str, ModuleInputs<'_>> = BTreeMap::new();
	let mut regular: Vec<MergePlanEntry> = Vec::new();
	let mut modules: BTreeMap<MergeUnitId, (DefinitionModulePolicy, ModuleInputs<'_>)> =
		BTreeMap::new();

	for (path, contributors) in &input.file_inventory {
		if let Some(database) = rules
			.map(|rules| rules.database_for(path))
			.transpose()?
			.flatten()
		{
			databases
				.entry(database)
				.or_default()
				.push((path, contributors));
			continue;
		}
		let Some(descriptor) = profile.classify_content_family(path) else {
			regular.push(classify_entry(
				path,
				contributors,
				None,
				&input.script_cache,
			));
			continue;
		};
		let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
			regular.push(classify_entry(
				path,
				contributors,
				Some(descriptor),
				&input.script_cache,
			));
			continue;
		};
		// A rule-covered family uses the rule's filename and directory selection.
		// Unmatched files must not create a second module at the same output path.
		if rules
			.map(|rules| rules.database_for(policy.output_path))
			.transpose()?
			.flatten()
			.is_some()
		{
			regular.push(classify_entry(
				path,
				contributors,
				Some(descriptor),
				&input.script_cache,
			));
			continue;
		}
		// A definition module is the directory itself, not the tree below it, so
		// a file in a subdirectory is not one of its inputs. It keeps its own
		// per-path handling instead of joining — and then blocking — the module.
		if !is_structural_merge_path(path, Some(descriptor))
			|| !path.is_child_of(policy.namespace_prefix)
		{
			regular.push(classify_entry(
				path,
				contributors,
				Some(descriptor),
				&input.script_cache,
			));
			continue;
		}
		let merge_unit = MergeUnitId {
			family_id: descriptor.id.as_str().to_string(),
			module_name: policy.namespace_prefix.file_name().to_string(),
		};
		modules
			.entry(merge_unit)
			.or_insert_with(|| (policy, Vec::new()))
			.1
			.push((path, contributors.as_slice()));
	}

	for (merge_unit, (policy, inputs)) in modules {
		let has_reset_participant = namespace_has_reset_participant(input, policy.namespace_prefix);
		if !module_has_non_base_contributor(&inputs) && !has_reset_participant {
			for (path, contributors) in inputs {
				let descriptor = profile.classify_content_family(path);
				regular.push(classify_entry(
					path,
					contributors,
					descriptor,
					&input.script_cache,
				));
			}
			continue;
		}
		regular.push(classify_module_entry(
			merge_unit,
			[policy],
			&inputs,
			input,
			&input.script_cache,
		)?);
	}
	for (database, inputs) in databases {
		let has_reset: bool = inputs.iter().any(|(path, _)| {
			path.parent()
				.is_some_and(|directory| namespace_has_reset_participant(input, directory))
		});
		if !module_has_non_base_contributor(&inputs) && !has_reset {
			for (path, contributors) in inputs {
				regular.push(classify_entry(
					path,
					contributors,
					profile.classify_content_family(path),
					&input.script_cache,
				));
			}
			continue;
		}
		match classify_database_entry(database, &inputs, input, profile)? {
			Some(entry) => regular.push(entry),
			None => {
				for (path, contributors) in inputs {
					regular.push(classify_entry(
						path,
						contributors,
						profile.classify_content_family(path),
						&input.script_cache,
					));
				}
			}
		}
	}
	regular.sort_by(|left, right| left.output_path().cmp(right.output_path()));
	Ok(regular)
}

/// Plan one database as a single merge unit that writes one file per
/// participating directory.
///
/// A database can be fed by several directories, and each keeps its own output
/// file and `replace_path` prefix: `replace_path` is declared per directory and
/// the extractors dispatch on the directory a definition was read from, so
/// folding a database into one file would apply one directory's semantics to
/// all of them. Writing each directory also avoids depending on the order in
/// which the loader reads them, which the extracted rules do not record.
/// `None` when the database has no definition-module directory at all, which
/// leaves its files to the per-path policies they had before the rules applied.
fn classify_database_entry(
	database: &str,
	inputs: &ModuleInputs<'_>,
	input: &ResolvedInput,
	profile: &Eu4,
) -> Result<Option<MergePlanEntry>, String> {
	let mut policies: BTreeMap<&GamePath, DefinitionModulePolicy> = BTreeMap::new();
	let mut unsupported: Vec<&GamePath> = Vec::new();
	for (path, _) in inputs {
		let descriptor: Option<&ContentFamilyDescriptor> = profile.classify_content_family(path);
		match descriptor.map(|descriptor| descriptor.load_policy) {
			// `database_for` matches only direct children of a rule directory,
			// so every input here is already inside its namespace.
			Some(ContentLoadPolicy::DefinitionModule(policy))
				if is_structural_merge_path(path, descriptor) =>
			{
				policies.insert(policy.namespace_prefix, policy);
			}
			_ => unsupported.push(path),
		}
	}
	let merge_unit: MergeUnitId = MergeUnitId {
		family_id: database.to_string(),
		module_name: database.to_string(),
	};
	// A database whose directories are not definition modules at all — EU4
	// 1.37.5 has one, `interface/state_view` — keeps the per-path merge its
	// content family already defines. Grouping it by database would withhold
	// content the analyzer merges today.
	if policies.is_empty() {
		return Ok(None);
	}
	if unsupported.is_empty() {
		return classify_module_entry(
			merge_unit,
			policies.values().copied(),
			inputs,
			input,
			&input.script_cache,
		)
		.map(Some);
	}
	// Only inputs the analyzer cannot merge structurally reach this point.
	// Deferring names them instead of reporting the whole database as opaque.
	let outputs: MergeModuleOutputs = module_outputs(policies.values().copied(), input)?;
	Ok(Some(MergePlanEntry {
		target: MergePlanTarget::Module {
			id: merge_unit,
			input_paths: inputs.iter().map(|(path, _)| (*path).to_owned()).collect(),
			outputs,
		},
		strategy: MergePlanStrategy::ManualConflict,
		contributors: module_contributors(inputs),
		winner: None,
		notes: vec![format!(
			"Database {database} cannot merge {} structurally; the complete database unit is deferred",
			unsupported
				.iter()
				.map(|path| path.as_str())
				.collect::<Vec<_>>()
				.join(", ")
		)],
	}))
}

/// One output per participating namespace, ordered by output path so the
/// primary output — the unit's review path and staging identity — is stable.
fn module_outputs(
	policies: impl IntoIterator<Item = DefinitionModulePolicy>,
	input: &ResolvedInput,
) -> Result<MergeModuleOutputs, String> {
	let mut outputs: Vec<MergeModuleOutput> = policies
		.into_iter()
		.map(|policy| {
			// `replace_path` is declared per directory, so each namespace
			// answers this for itself.
			let replace_prefix = (policy.output_mode == DefinitionModuleOutput::ReplaceNamespace
				|| namespace_has_reset_participant(input, policy.namespace_prefix))
			.then(|| policy.namespace_prefix.to_owned());
			MergeModuleOutput::new(policy.output_path.to_owned(), replace_prefix).ok_or_else(|| {
				format!(
					"definition module output {} is not inside a namespace directory",
					policy.output_path
				)
			})
		})
		.collect::<Result<_, String>>()?;
	outputs.sort_by(|left, right| left.output_path().cmp(right.output_path()));
	outputs.dedup_by(|left, right| left.output_path() == right.output_path());
	MergeModuleOutputs::new(outputs)
		.ok_or_else(|| "a definition module unit has no namespace".to_string())
}

fn module_has_non_base_contributor(inputs: &ModuleInputs<'_>) -> bool {
	inputs.iter().any(|(_, contributors)| {
		contributors
			.iter()
			.any(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
	})
}

/// Whether a mod in the playset declares a `replace_path` that resets
/// `namespace`: the namespace itself or a directory above it.
fn namespace_has_reset_participant(input: &ResolvedInput, namespace: &GamePath) -> bool {
	input.mods.iter().any(|mod_item| {
		mod_item.root_path.is_some()
			&& mod_item.descriptor.as_ref().is_some_and(|descriptor| {
				descriptor
					.replace_path
					.iter()
					.any(|replace_path| namespace.starts_with(replace_path))
			})
	})
}

fn classify_module_entry(
	merge_unit: MergeUnitId,
	policies: impl IntoIterator<Item = DefinitionModulePolicy>,
	inputs: &ModuleInputs<'_>,
	input: &ResolvedInput,
	script_cache: &InputScriptCache,
) -> Result<MergePlanEntry, String> {
	let outputs: MergeModuleOutputs = module_outputs(policies, input)?;
	let input_paths = inputs
		.iter()
		.map(|(path, _)| (*path).to_owned())
		.collect::<Vec<_>>();
	let contributors: Vec<MergePlanContributor> = module_contributors(inputs);
	let mut notes = Vec::new();
	let strategy = inputs
		.iter()
		.find_map(|(input_path, contributors)| {
			validate_structural_merge_inputs(input_path, contributors, script_cache).err()
		})
		.map_or(MergePlanStrategy::StructuralMerge, |error| {
			notes.push(error.to_string());
			MergePlanStrategy::ManualConflict
		});
	let winner = (strategy != MergePlanStrategy::ManualConflict)
		.then(|| contributors.last().cloned())
		.flatten();

	Ok(MergePlanEntry {
		target: MergePlanTarget::Module {
			id: merge_unit,
			input_paths,
			outputs,
		},
		strategy,
		contributors,
		winner,
		notes,
	})
}

/// Every contributor of a module, ordered by precedence, then by its native
/// source path, then by mod. Render paths only after ordering: display text
/// can collapse distinct non-UTF-8 root names.
fn module_contributors(inputs: &ModuleInputs<'_>) -> Vec<MergePlanContributor> {
	let mut contributors: Vec<(PathBuf, &ResolvedInputContributor)> = inputs
		.iter()
		.flat_map(|(_, contributors)| contributors.iter())
		.map(|contributor| (contributor.absolute_path(), contributor))
		.collect();
	contributors.sort_by(|(left_path, left), (right_path, right)| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left_path.cmp(right_path))
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	contributors
		.into_iter()
		.map(|(_, contributor)| to_merge_contributor(contributor))
		.collect()
}

fn classify_entry(
	path: &GamePath,
	contributors: &[ResolvedInputContributor],
	descriptor: Option<&ContentFamilyDescriptor>,
	script_cache: &InputScriptCache,
) -> MergePlanEntry {
	let contributors_out: Vec<MergePlanContributor> =
		contributors.iter().map(to_merge_contributor).collect();
	let mut winner = contributors_out.last().cloned();
	let mut notes = Vec::new();

	let strategy = if contributors.len() == 1 {
		MergePlanStrategy::CopyThrough
	} else if is_structural_merge_path(path, descriptor) {
		match validate_structural_merge_inputs(path, contributors, script_cache) {
			Ok(()) => MergePlanStrategy::StructuralMerge,
			Err(err) => {
				notes.push(err.to_string());
				MergePlanStrategy::ManualConflict
			}
		}
	} else if is_localisation_yml_path(path) {
		MergePlanStrategy::LocalisationMerge
	} else {
		// Text-like or binary content with no structural-merge handler:
		// last-writer-overlay matches what the game's load order would do
		// at runtime (later-precedence mod replaces earlier ones).
		if !is_text_like_overlay_path(path) {
			notes.push("binary overlap resolved by last-writer-overlay".to_string());
		}
		MergePlanStrategy::LastWriterOverlay
	};

	if strategy == MergePlanStrategy::ManualConflict {
		winner = None;
	}

	MergePlanEntry {
		target: MergePlanTarget::File {
			path: path.to_owned(),
		},
		strategy,
		contributors: contributors_out,
		winner,
		notes,
	}
}

/// The plan's view of a contributor. `source_path` renders the physical file
/// for display only; the contributor is identified by its mod, precedence and
/// base-game flag (see `ResolvedInputContributor::is_planned_as`). The file is
/// spelled with its components joined by `/`, and as the host displays it when
/// it has no such spelling, never with a name's `\` read as a separator.
fn to_merge_contributor(contributor: &ResolvedInputContributor) -> MergePlanContributor {
	let path: PathBuf = contributor.absolute_path();
	MergePlanContributor {
		mod_id: contributor.mod_id.clone(),
		source_path: slash_path_text(&path).unwrap_or_else(|_| path.display().to_string()),
		precedence: contributor.precedence,
		is_base_game: contributor.is_base_game,
	}
}

fn summarize_paths(paths: &[MergePlanEntry]) -> MergePlanStrategies {
	let mut strategies = MergePlanStrategies {
		total_paths: paths.len(),
		..MergePlanStrategies::default()
	};

	for path in paths {
		match path.strategy {
			MergePlanStrategy::CopyThrough => strategies.copy_through += 1,
			MergePlanStrategy::LastWriterOverlay => strategies.last_writer_overlay += 1,
			MergePlanStrategy::StructuralMerge => strategies.structural_merge += 1,
			MergePlanStrategy::LocalisationMerge => strategies.localisation_merge += 1,
			MergePlanStrategy::ManualConflict => strategies.manual_conflict += 1,
		}
	}

	strategies
}

fn current_generated_at() -> String {
	let millis = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |duration| duration.as_millis());
	millis.to_string()
}

fn validate_structural_merge_inputs(
	path: &GamePath,
	contributors: &[ResolvedInputContributor],
	script_cache: &InputScriptCache,
) -> Result<(), MergeError> {
	let mut failures = Vec::new();
	let is_defines_path = is_clausewitz_defines_path(path);

	for contributor in contributors {
		let parse_ok = script_cache
			.overlay_parse_ok(&contributor.mod_id, path)
			.or(contributor.parse_ok_hint);
		let Some(parse_ok) = parse_ok else {
			failures.push(format!(
				"missing cached parse status for {}",
				contributor.mod_id
			));
			continue;
		};
		if !parse_ok {
			if contributor.is_base_game {
				failures.push(format!("base game parse issues in {}", contributor.mod_id));
			} else {
				failures.push(format!("cached parse issues in {}", contributor.mod_id));
			}
			continue;
		}
		if !is_defines_path {
			continue;
		}
		let parsed = match script_cache.load(contributor) {
			Ok(parsed) => parsed,
			Err(error) => {
				failures.push(format!(
					"failed to load AST for {}: {error}",
					contributor.mod_id
				));
				continue;
			}
		};
		if let Err(err) = normalize_defines_file(&parsed) {
			failures.push(format!(
				"non-normalizable defines in {}: {}",
				contributor.mod_id, err
			));
		}
	}

	if failures.is_empty() {
		Ok(())
	} else {
		Err(MergeError::Validation {
			subject: Some(MergeErrorSubject::Game(path.to_owned())),
			message: format!(
				"structural merge blocked by invalid contributors: {}",
				failures.join(", ")
			),
		})
	}
}

fn is_structural_merge_path(path: &GamePath, descriptor: Option<&ContentFamilyDescriptor>) -> bool {
	classify_document_family(path) == Some(DocumentFamily::Clausewitz)
		&& descriptor
			.and_then(|descriptor| descriptor.merge_key_source)
			.is_some()
}

fn validate_structural_snapshot(
	input: &ResolvedInput,
	profile: &Eu4,
	entries: &[MergePlanEntry],
) -> Result<(), String> {
	for (entry, path) in entries.iter().flat_map(|entry| {
		entry
			.target
			.input_paths()
			.iter()
			.map(move |path| (entry, path))
	}) {
		let contributors: &[ResolvedInputContributor] =
			input.file_inventory.get(path).ok_or_else(|| {
				format!("merge plan input {path} is missing from the input inventory")
			})?;
		let descriptor = profile.classify_content_family(path);
		if !is_structural_merge_path(path, descriptor) {
			continue;
		}
		// Untouched vanilla modules remain copy-through; every structural input
		// of a participating unit must have a parse status, even across directories.
		if entry.target.module_id().is_none()
			&& contributors
				.iter()
				.all(|contributor| contributor.is_base_game || contributor.is_synthetic_base)
			&& descriptor.is_some_and(|descriptor| {
				matches!(
					descriptor.load_policy,
					ContentLoadPolicy::DefinitionModule(_)
				)
			}) {
			continue;
		}
		for contributor in contributors {
			if contributor.parse_ok_hint.is_some() {
				continue;
			}
			let repair = if contributor.is_base_game {
				"foch data build eu4 --from-game-path <EU4_ROOT> --game-version auto --install"
			} else {
				"foch cache clear --layer mods --yes"
			};
			return Err(format!(
				"structural merge snapshot invariant violated for {path} from {}: missing parse status; run `{repair}` and retry",
				contributor.mod_id
			));
		}
	}
	Ok(())
}

pub(crate) fn prune_noop_script_contributors(input: &mut ResolvedInput, profile: &Eu4) {
	let script_cache = &input.script_cache;
	let mut emptied = Vec::new();
	for (relative_path, contributors) in &mut input.file_inventory {
		let descriptor = profile.classify_content_family(relative_path);
		if descriptor.is_some_and(|descriptor| {
			matches!(
				descriptor.load_policy,
				ContentLoadPolicy::DefinitionModule(_)
			)
		}) {
			continue;
		}
		if !is_structural_merge_path(relative_path, descriptor) {
			continue;
		}
		if contributors.len() < 2 {
			continue;
		}
		contributors.retain(|contributor| {
			contributor.is_base_game
				|| contributor.is_synthetic_base
				|| script_cache.is_noop_hint(&contributor.mod_id, relative_path) != Some(true)
		});
		if contributors.is_empty() {
			emptied.push(relative_path.clone());
		}
	}
	for relative_path in emptied {
		input.file_inventory.remove(&relative_path);
	}
}

/// The lowercased extension of the final component; `None` for a name with
/// no `.` after its first character.
fn lowercase_extension(path: &GamePath) -> Option<String> {
	path.extension().map(str::to_ascii_lowercase)
}

fn is_text_like_overlay_path(path: &GamePath) -> bool {
	lowercase_extension(path).is_some_and(|ext| {
		matches!(
			ext.as_str(),
			"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
		)
	})
}

/// Localisation YAML files (`localisation/**.yml` and
/// `common/localisation/**.yml`, directory names compared ignoring ASCII case)
/// follow the EU4 paradox-yaml format and can be merged at the key level: the
/// union of all contributors' keys is preserved, with the highest-precedence
/// contributor winning on collision.
pub(crate) fn is_localisation_yml_path(path: &GamePath) -> bool {
	let under_loc = path.is_inside(&["localisation"], str::eq_ignore_ascii_case)
		|| path.is_inside(&["common", "localisation"], str::eq_ignore_ascii_case);
	under_loc && lowercase_extension(path).is_some_and(|ext| matches!(ext.as_str(), "yml" | "yaml"))
}

#[cfg(test)]
mod tests {
	use super::{build_merge_plan_from_input, to_merge_contributor};
	use crate::game::eu4::Eu4;
	use crate::input::{ResolvedInput, ResolvedInputContributor};
	use crate::model::{
		GamePath, GamePathBuf, MergePlanContributor, MergePlanStrategy, MergePlanTarget,
		ModCandidate,
	};
	use crate::playset::descriptor::ModDescriptor;
	use crate::playset::{Playset, PlaysetEntry};
	use std::collections::{BTreeMap, BTreeSet};
	use std::path::PathBuf;

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn texts<'a>(paths: impl IntoIterator<Item = &'a GamePath>) -> Vec<&'a str> {
		paths.into_iter().map(GamePath::as_str).collect()
	}

	fn input_with_snapshot_gap(
		mod_id: &str,
		is_base_game: bool,
		parse_ok_hint: Option<bool>,
	) -> ResolvedInput {
		input_with_snapshot_gap_at_path("events/test.txt", mod_id, is_base_game, parse_ok_hint)
	}

	fn input_with_snapshot_gap_at_path(
		relative_path: &str,
		mod_id: &str,
		is_base_game: bool,
		parse_ok_hint: Option<bool>,
	) -> ResolvedInput {
		let root_path = PathBuf::from(mod_id);
		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(
			game_path(relative_path),
			vec![ResolvedInputContributor {
				mod_id: mod_id.to_string(),
				root_path: root_path.clone(),
				relative_path: game_path(relative_path),
				precedence: usize::from(!is_base_game),
				is_base_game,
				is_synthetic_base: false,
				parse_ok_hint,
				mod_hash: (!is_base_game).then(|| format!("hash-{mod_id}")),
			}],
		);
		ResolvedInput {
			playlist_path: PathBuf::from("playlist.json"),
			playlist: Playset {
				game: Eu4,
				name: "snapshot-gap".to_string(),
				mods: Vec::new(),
			},
			mods: Vec::new(),
			installed_base_snapshot: None,
			game_version: None,
			cache_game_version: None,
			mod_snapshots: Vec::new(),
			script_cache: Default::default(),
			file_inventory,
			verified_absent_base_paths: BTreeSet::new(),
			requested_retained_paths: None,
			effective_retained_paths: None,
		}
	}

	fn mod_contributor(
		mod_id: &str,
		relative_path: &str,
		precedence: usize,
	) -> ResolvedInputContributor {
		ResolvedInputContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(mod_id),
			relative_path: game_path(relative_path),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some(format!("hash-{mod_id}")),
		}
	}

	fn reset_only_mod(mod_id: &str, replace_path: &str) -> ModCandidate {
		let root_path = PathBuf::from(mod_id);
		ModCandidate {
			entry: PlaysetEntry {
				enabled: true,
				root_path: Some(root_path.clone()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: Some(root_path),
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: mod_id.to_string(),
				replace_path: vec![game_path(replace_path)],
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	#[test]
	fn database_rules_merge_cross_directory_inputs_into_one_unit_per_directory_output() {
		let event_path: &str = "common/event_modifiers/a.txt";
		let static_path: &str = "common/static_modifiers/b.txt";
		let mut input: ResolvedInput =
			input_with_snapshot_gap_at_path(event_path, "mod-b", false, Some(true));
		input.game_version = Some("1.37.5".to_string());
		input.file_inventory.insert(
			game_path(static_path),
			vec![mod_contributor("mod-a", static_path, 2)],
		);

		let result = build_merge_plan_from_input(&input, false);
		assert!(!result.has_fatal_errors(), "{:?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1);
		let entry: &crate::model::MergePlanEntry = &result.paths[0];
		assert_eq!(
			texts(
				entry
					.target
					.input_paths()
					.iter()
					.map(GamePathBuf::as_game_path)
			),
			[event_path, static_path]
		);
		assert_eq!(
			entry.target.module_id().unwrap().module_name,
			"CStaticModifierDataBase"
		);
		// One review unit, one output file per contributing directory: both
		// mods' contributions are kept instead of deferring the database.
		assert_eq!(entry.strategy, MergePlanStrategy::StructuralMerge);
		assert_eq!(
			texts(entry.target.output_paths()),
			[
				"common/event_modifiers/zzz_foch_event_modifiers.txt",
				"common/static_modifiers/zzz_foch_static_modifiers.txt",
			]
		);
		// Each namespace keeps only the inputs read from its own directory.
		assert_eq!(
			texts(
				entry
					.target
					.namespace_input_paths(&game_path("common/event_modifiers"))
			),
			[event_path]
		);
		assert_eq!(
			texts(
				entry
					.target
					.namespace_input_paths(&game_path("common/static_modifiers"))
			),
			[static_path]
		);
		assert!(
			entry
				.target
				.module_outputs()
				.iter()
				.all(|output| output.replace_prefix().is_none())
		);
		assert_eq!(
			entry
				.contributors
				.iter()
				.map(|contributor| contributor.mod_id.as_str())
				.collect::<Vec<_>>(),
			["mod-b", "mod-a"]
		);

		input.game_version = Some("1.37.4".to_string());
		let unsupported_version = build_merge_plan_from_input(&input, false);
		assert_eq!(unsupported_version.paths.len(), 2);
	}

	#[test]
	fn database_units_keep_cross_directory_vanilla_and_validate_its_snapshot() {
		let base_path: &str = "common/event_modifiers/base.txt";
		let mod_path: &str = "common/static_modifiers/mod.txt";
		let other_path: &str = "common/policies/mod.txt";
		let mut input: ResolvedInput =
			input_with_snapshot_gap_at_path(base_path, "__game__eu4", true, Some(true));
		input.game_version = Some("1.37.5".to_string());
		for path in [mod_path, other_path] {
			input
				.file_inventory
				.insert(game_path(path), vec![mod_contributor("mod-a", path, 1)]);
		}
		let result = build_merge_plan_from_input(&input, true);
		assert!(!result.has_fatal_errors(), "{:?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 2);
		let unit = result
			.paths
			.iter()
			.find(|entry| {
				entry
					.target
					.input_paths()
					.iter()
					.any(|path| path.as_str() == mod_path)
			})
			.unwrap();
		assert_eq!(
			texts(
				unit.target
					.input_paths()
					.iter()
					.map(GamePathBuf::as_game_path)
			),
			[base_path, mod_path]
		);
		assert!(unit.contributors[0].is_base_game);
		assert_eq!(unit.contributors[0].precedence, 0);
		assert_eq!(
			unit.contributors[1].source_path,
			format!("mod-a/{mod_path}")
		);
		assert!(result.paths.iter().any(|entry| {
			entry.target.module_id().unwrap().module_name == "CPolicyDatabase"
				&& entry.target.input_paths() == [game_path(other_path)]
		}));

		input
			.file_inventory
			.get_mut(GamePath::new(base_path).expect("valid game path"))
			.unwrap()[0]
			.parse_ok_hint = None;
		let invalid = build_merge_plan_from_input(&input, true);
		assert!(invalid.has_fatal_errors());
		assert!(invalid.paths.is_empty());
		assert!(invalid.fatal_errors[0].contains(base_path));
		assert!(invalid.fatal_errors[0].contains("foch data build"));
	}

	#[test]
	fn database_rules_keep_existing_single_directory_merge_behavior_and_filter_filenames() {
		let first: &str = "common/static_modifiers/a.txt";
		let second: &str = "common/static_modifiers/b.txt";
		let unmatched: &str = "common/static_modifiers/notes.gui";
		let mut input: ResolvedInput =
			input_with_snapshot_gap_at_path(first, "mod-a", false, Some(true));
		input.game_version = Some("1.37.5".to_string());
		for path in [second, unmatched] {
			input
				.file_inventory
				.insert(game_path(path), vec![mod_contributor("mod-b", path, 2)]);
		}

		let result = build_merge_plan_from_input(&input, false);
		assert!(!result.has_fatal_errors(), "{:?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 2);
		let module = result
			.paths
			.iter()
			.find(|entry| entry.target.module_id().is_some())
			.unwrap();
		assert_eq!(
			texts(
				module
					.target
					.input_paths()
					.iter()
					.map(GamePathBuf::as_game_path)
			),
			[first, second]
		);
		assert_eq!(
			module.target.module_id().unwrap().module_name,
			"CStaticModifierDataBase"
		);
		assert_eq!(module.strategy, MergePlanStrategy::StructuralMerge);
		assert_eq!(
			module.output_path().as_str(),
			"common/static_modifiers/zzz_foch_static_modifiers.txt"
		);
		assert!(result.paths.iter().any(|entry| matches!(
			&entry.target,
			MergePlanTarget::File { path } if path.as_str() == unmatched
		)));
	}

	#[test]
	fn database_base_only_files_remain_copy_through_until_a_reset_participates() {
		let mut input: ResolvedInput = input_with_snapshot_gap_at_path(
			"common/static_modifiers/base.txt",
			"__game__eu4",
			true,
			Some(true),
		);
		input.game_version = Some("1.37.5".to_string());
		let base_only = build_merge_plan_from_input(&input, true);
		assert_eq!(base_only.paths[0].strategy, MergePlanStrategy::CopyThrough);
		input
			.mods
			.push(reset_only_mod("reset-mod", "common/static_modifiers"));
		let reset = build_merge_plan_from_input(&input, true);
		assert!(!reset.has_fatal_errors(), "{:?}", reset.fatal_errors);
		assert_eq!(reset.paths[0].strategy, MergePlanStrategy::StructuralMerge);
		assert_eq!(
			reset.paths[0].target.replace_prefix().map(GamePath::as_str),
			Some("common/static_modifiers")
		);
	}

	#[test]
	fn missing_mod_parse_status_is_a_fatal_snapshot_invariant() {
		let input = input_with_snapshot_gap("mod-a", false, None);

		let result = build_merge_plan_from_input(&input, false);

		assert!(result.has_fatal_errors());
		assert!(result.paths.is_empty());
		assert!(
			result
				.fatal_errors
				.iter()
				.any(|error| error.contains("foch cache clear --layer mods --yes"))
		);
	}

	#[test]
	fn missing_cached_ast_is_not_a_snapshot_invariant() {
		let input = input_with_snapshot_gap("__game__", true, Some(true));

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors());
		assert_eq!(result.paths.len(), 1);
	}

	#[test]
	fn singleton_structural_contributor_does_not_load_an_ast_during_pruning() {
		let input = input_with_snapshot_gap("mod-a", false, Some(true));

		let result = build_merge_plan_from_input(&input, false);

		assert!(!result.has_fatal_errors());
		assert_eq!(result.paths.len(), 1);
		assert!(!input.script_cache.is_loaded(
			"mod-a",
			GamePath::new("events/test.txt").expect("valid game path")
		));
	}

	#[test]
	fn base_only_definition_module_input_is_planned_as_copy_through() {
		let input = input_with_snapshot_gap_at_path(
			"common/powerprojection/00_static.txt",
			"__game__eu4",
			true,
			None,
		);

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		assert_eq!(result.paths[0].strategy, MergePlanStrategy::CopyThrough);
		assert!(matches!(
			&result.paths[0].target,
			MergePlanTarget::File { path }
				if path.as_str() == "common/powerprojection/00_static.txt"
		));
		assert!(
			result.paths[0]
				.contributors
				.iter()
				.all(|contributor| contributor.is_base_game)
		);
	}

	#[test]
	fn participating_definition_module_keeps_base_only_inputs() {
		let base_path = "common/powerprojection/00_static.txt";
		let mod_path = "common/powerprojection/modded.txt";
		let mut input = input_with_snapshot_gap_at_path(base_path, "__game__eu4", true, Some(true));
		input.file_inventory.insert(
			game_path(mod_path),
			vec![mod_contributor("mod-a", mod_path, 1)],
		);

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		let MergePlanTarget::Module {
			input_paths,
			outputs,
			..
		} = &result.paths[0].target
		else {
			panic!("expected definition module");
		};
		assert_eq!(input_paths, &[game_path(base_path), game_path(mod_path)]);
		assert!(outputs[0].replace_prefix().is_none());
		assert!(
			result.paths[0]
				.contributors
				.iter()
				.any(|contributor| contributor.is_base_game)
		);
	}

	#[test]
	fn reset_only_mod_participates_in_base_backed_definition_module() {
		let mut input = input_with_snapshot_gap_at_path(
			"common/powerprojection/00_static.txt",
			"__game__eu4",
			true,
			Some(true),
		);
		input.mods.push(reset_only_mod("reset-mod", "common"));
		input.mod_snapshots.push(None);

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		assert!(matches!(
			&result.paths[0].target,
			MergePlanTarget::Module { outputs, .. }
				if outputs[0].replace_prefix().map(GamePath::as_str) == Some("common/powerprojection")
		));
	}

	#[test]
	fn non_clausewitz_lua_under_structural_family_does_not_require_parse_status() {
		let mut input =
			input_with_snapshot_gap_at_path("gfx/shader_upgrade.lua", "__game__eu4", true, None);
		input
			.file_inventory
			.get_mut(GamePath::new("gfx/shader_upgrade.lua").expect("valid game path"))
			.expect("shader inventory")
			.push(ResolvedInputContributor {
				mod_id: "mod-a".to_string(),
				root_path: PathBuf::from("mod-a"),
				relative_path: game_path("gfx/shader_upgrade.lua"),
				precedence: 1,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: None,
				mod_hash: Some("hash-mod-a".to_string()),
			});

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.strategies.last_writer_overlay, 1);
		assert_eq!(
			result.paths[0]
				.winner
				.as_ref()
				.map(|winner| winner.mod_id.as_str()),
			Some("mod-a")
		);
	}

	#[test]
	fn clausewitz_paths_under_mixed_extension_families_require_parse_status() {
		for path in [
			"gfx/test.gfx",
			"common/defines.lua",
			"common/defines/test.lua",
		] {
			let input = input_with_snapshot_gap_at_path(path, "__game__eu4", true, None);

			let result = build_merge_plan_from_input(&input, true);

			assert!(result.has_fatal_errors(), "{path}");
			assert!(result.paths.is_empty(), "{path}");
		}
	}

	#[test]
	fn non_clausewitz_file_under_definition_module_is_a_regular_overlay() {
		let mut input = input_with_snapshot_gap_at_path(
			"common/governments/metadata.json",
			"__game__eu4",
			true,
			None,
		);
		input
			.file_inventory
			.get_mut(GamePath::new("common/governments/metadata.json").expect("valid game path"))
			.expect("governments metadata inventory")
			.push(ResolvedInputContributor {
				mod_id: "mod-a".to_string(),
				root_path: PathBuf::from("mod-a"),
				relative_path: game_path("common/governments/metadata.json"),
				precedence: 1,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: None,
				mod_hash: Some("hash-mod-a".to_string()),
			});

		let result = build_merge_plan_from_input(&input, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.strategies.last_writer_overlay, 1);
		assert!(matches!(
			&result.paths[0].target,
			crate::model::MergePlanTarget::File { path }
				if path.as_str() == "common/governments/metadata.json"
		));
	}

	/// Two mods that share a precedence (a priority boost can do that) are
	/// ordered by their native source paths, then by mod.
	#[test]
	fn module_contributors_sharing_a_precedence_follow_their_native_source_paths() {
		let order = |alpha_root: &str, beta_root: &str| {
			let mut input = input_with_snapshot_gap_at_path(
				"common/governments/a.txt",
				"alpha",
				false,
				Some(true),
			);
			let alpha = &mut input
				.file_inventory
				.get_mut(GamePath::new("common/governments/a.txt").expect("valid game path"))
				.expect("alpha inventory")[0];
			alpha.root_path = PathBuf::from(alpha_root);
			let mut beta = mod_contributor("beta", "common/governments/b.txt", 1);
			beta.root_path = PathBuf::from(beta_root);
			input
				.file_inventory
				.insert(game_path("common/governments/b.txt"), vec![beta]);
			let result = build_merge_plan_from_input(&input, false);
			assert!(!result.has_fatal_errors(), "{:?}", result.fatal_errors);
			assert!(result.paths[0].target.module_id().is_some());
			result.paths[0]
				.contributors
				.iter()
				.map(|contributor| (contributor.mod_id.clone(), contributor.precedence))
				.collect::<Vec<_>>()
		};

		let alpha = ("alpha".to_string(), 1);
		let beta = ("beta".to_string(), 1);
		assert_eq!(
			order("/zzz/alpha", "/aaa/beta"),
			[beta.clone(), alpha.clone()]
		);
		assert_eq!(order("/aaa/alpha", "/zzz/beta"), [alpha, beta]);
	}

	/// `source_path` spells the physical file with its components joined by
	/// `/`: the host text of a plain Unix path, and a drive path with `/` for
	/// its `\` separators.
	#[test]
	fn source_path_joins_the_physical_components_with_slashes() {
		let render = |root: &str| {
			let mut contributor = mod_contributor("123", "common/x.txt", 1);
			contributor.root_path = PathBuf::from(root);
			to_merge_contributor(&contributor).source_path
		};
		assert_eq!(
			render("/steam/steamapps/workshop/content/236850/123"),
			"/steam/steamapps/workshop/content/236850/123/common/x.txt"
		);
		#[cfg(windows)]
		assert_eq!(
			render(r"C:\Steam\steamapps\workshop\content\236850\123"),
			"C:/Steam/steamapps/workshop/content/236850/123/common/x.txt"
		);
	}

	/// On Unix a `\` inside a root's name is a character, not a separator, so
	/// the root `/mods/a\b` is not rendered as the different directory
	/// `/mods/a/b`.
	#[cfg(unix)]
	#[test]
	fn source_path_keeps_a_backslash_inside_a_root_name() {
		let render = |root: &str| {
			let mut contributor = mod_contributor("mod-a", "common/x.txt", 1);
			contributor.root_path = PathBuf::from(root);
			to_merge_contributor(&contributor).source_path
		};
		assert_eq!(render("/mods/a\\b"), "/mods/a\\b/common/x.txt");
		assert_eq!(render("/mods/a/b"), "/mods/a/b/common/x.txt");
	}

	#[cfg(unix)]
	#[test]
	fn module_order_does_not_compare_lossy_root_displays() {
		use std::ffi::OsString;
		use std::os::unix::ffi::OsStringExt;

		// In-memory paths: the filesystem need not support these root names.
		let path: GamePathBuf = game_path("common/governments/a.txt");
		let mut alpha: ResolvedInputContributor = mod_contributor("alpha", path.as_str(), 1);
		let mut beta: ResolvedInputContributor = mod_contributor("beta", path.as_str(), 1);
		alpha.root_path = PathBuf::from(OsString::from_vec(b"/mods/\xff".to_vec()));
		beta.root_path = PathBuf::from(OsString::from_vec(b"/mods/\xfe".to_vec()));
		assert_eq!(
			to_merge_contributor(&alpha).source_path,
			to_merge_contributor(&beta).source_path,
			"display text alone cannot order these roots"
		);
		let contributors: [ResolvedInputContributor; 2] = [alpha, beta];
		let inputs: super::ModuleInputs<'_> = vec![(&path, &contributors)];
		let ordered: Vec<MergePlanContributor> = super::module_contributors(&inputs);
		assert_eq!(ordered[0].mod_id, "beta");
		assert_eq!(ordered[1].mod_id, "alpha");
	}

	/// At precedence 0 the base game meets the synthetic bases of files it does
	/// not ship, and they are ordered by their native source paths. In the default
	/// Steam layout the game under `steamapps/common/` comes before every
	/// synthetic base under `steamapps/workshop/`, which follow by mod; where
	/// the game sorts after the mods, it follows the synthetic bases.
	#[test]
	fn module_contributors_at_precedence_zero_follow_their_native_source_paths() {
		const BASE: &str = "__game__eu4";
		let plan_order = |root_of: &dyn Fn(&str) -> PathBuf| {
			let contributor = |mod_id: &str, path: &str, precedence: usize| {
				let mut contributor = mod_contributor(mod_id, path, precedence);
				contributor.root_path = root_of(mod_id);
				contributor.is_base_game = mod_id == BASE;
				contributor
			};
			let synthetic = |mod_id: &str, path: &str| ResolvedInputContributor {
				is_synthetic_base: true,
				..contributor(mod_id, path, 0)
			};
			let (vanilla, shared_early, shared_late) = (
				"common/governments/00_governments.txt",
				"common/governments/00_ME_governments.txt",
				"common/governments/zz_a.txt",
			);
			let mut input = input_with_snapshot_gap_at_path(vanilla, BASE, true, Some(true));
			input
				.file_inventory
				.insert(game_path(vanilla), vec![contributor(BASE, vanilla, 0)]);
			input.file_inventory.insert(
				game_path(shared_early),
				vec![
					synthetic("2000", shared_early),
					contributor("2000", shared_early, 1),
					contributor("1000", shared_early, 2),
				],
			);
			input.file_inventory.insert(
				game_path(shared_late),
				vec![
					synthetic("1000", shared_late),
					contributor("1000", shared_late, 2),
					contributor("3000", shared_late, 3),
				],
			);
			let result = build_merge_plan_from_input(&input, false);
			assert!(!result.has_fatal_errors(), "{:?}", result.fatal_errors);
			assert_eq!(result.paths.len(), 1);
			assert!(result.paths[0].target.module_id().is_some());
			result.paths[0].contributors.clone()
		};
		let identity = |contributors: &[MergePlanContributor]| {
			contributors
				.iter()
				.map(|contributor| {
					(
						contributor.mod_id.clone(),
						contributor.precedence,
						contributor
							.source_path
							.rsplit('/')
							.next()
							.map(str::to_string),
					)
				})
				.collect::<Vec<_>>()
		};
		let expected: Vec<(String, usize, Option<String>)> = [
			(BASE, 0, "00_governments.txt"),
			("1000", 0, "zz_a.txt"),
			("2000", 0, "00_ME_governments.txt"),
			("2000", 1, "00_ME_governments.txt"),
			("1000", 2, "00_ME_governments.txt"),
			("1000", 2, "zz_a.txt"),
			("3000", 3, "zz_a.txt"),
		]
		.into_iter()
		.map(|(mod_id, precedence, file)| (mod_id.to_string(), precedence, Some(file.to_string())))
		.collect();

		let steam = |mod_id: &str| {
			if mod_id == BASE {
				PathBuf::from("/steam/steamapps/common/Europa Universalis IV")
			} else {
				PathBuf::from("/steam/steamapps/workshop/content/236850").join(mod_id)
			}
		};
		assert_eq!(identity(&plan_order(&steam)), expected);

		let elsewhere = |mod_id: &str| {
			if mod_id == BASE {
				PathBuf::from("/zzz/game")
			} else {
				PathBuf::from("/aaa/mods").join(mod_id)
			}
		};
		let mut game_last = expected.clone();
		let game = game_last.remove(0);
		game_last.insert(2, game);
		assert_eq!(identity(&plan_order(&elsewhere)), game_last);
	}

	/// Localisation directories are matched ignoring ASCII case, one component
	/// at a time, and extensions ignoring case, as the lowercased text was. The
	/// extension is the game path's own: a name that is only an extension
	/// (`.yml`) has none, and neither has a root file named `txt`.
	#[test]
	fn overlay_classes_come_from_the_directory_and_extension_of_the_path() {
		for (path, localisation, text_like) in [
			("localisation/x.yml", true, true),
			("Localisation/X.YML", true, true),
			("common/LOCALISATION/x.yaml", true, true),
			("localisation/nested/x.yml", true, true),
			("localisation/x.txt", false, true),
			("common/localisation_extra/x.yml", false, true),
			("localisation/.yml", false, false),
			("gfx/x.TXT", false, true),
			("gfx/x.dds", false, false),
			("gfx/x", false, false),
			("txt", false, false),
		] {
			let path = game_path(path);
			assert_eq!(
				super::is_localisation_yml_path(&path),
				localisation,
				"{path}"
			);
			assert_eq!(super::is_text_like_overlay_path(&path), text_like, "{path}");
		}
	}

	/// A `replace_path` resets a namespace when it names the namespace itself or
	/// a directory above it, compared by whole components.
	#[test]
	fn a_replace_path_covers_its_directory_and_what_lies_below_it() {
		let mut input = input_with_snapshot_gap("mod-a", false, Some(true));
		for (replace_path, namespace, covered) in [
			("common/ideas", "common/ideas", true),
			("common", "common/ideas", true),
			("common/ideas", "common/ideas/nested", true),
			("common/ideas", "common/ideas_extra", false),
			("common/ideas_extra", "common/ideas", false),
			("common/ideas/nested", "common/ideas", false),
		] {
			input.mods = vec![reset_only_mod("reset", replace_path)];
			assert_eq!(
				super::namespace_has_reset_participant(&input, &game_path(namespace)),
				covered,
				"{replace_path} over {namespace}"
			);
		}
	}
}
