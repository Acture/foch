//! Merge integration: migrate each mod's GUI actions to its main decision.
//!
//! With `[gui] mode = "decisions"` the adapter reads the playset's effective
//! interface, custom GUI, scripted triggers and effects, localisation,
//! customizable localisation and map groups, and for each mod whose
//! `custom_button` actions have a decision route emits one connected output
//! group: the generated decision, events, scripts and localisation, and the
//! mod's custom GUI definitions with each migrated button hidden
//! (`potential = { always = no }`). Layout files are untouched, so the
//! interface merge is unaffected. Every non-base file read is bound to the
//! bytes analysis saw. Actions the route refuses stay in the GUI and are
//! reported on the mod's custom GUI units.

use crate::model::{GamePath, GamePathBuf};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::Arc;

use super::decision::{assign, block, identifier};
use super::geography::Geography;
use super::{
	FeatureCatalog, GeneratedDecision, GuiOverrides, GuiSources, RouteContext, ScriptSource,
	SourceMod,
};
use crate::game::eu4::derived_localisation::{CustomLocalisation, EffectiveLocalisation};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::emit::emit_clausewitz_statements_with_options;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue, parse_clausewitz_content};
use crate::game::eu4::text::decode_paradox_bytes;
use crate::input::{ResolvedInput, ResolvedInputContributor};
use crate::merge::error::MergeError;
use crate::merge::transform::input::{ReviewedInputs, SourceGuard};
use crate::merge::transform::{
	EmittedOutput, GeneratedArtifact, OutputValidation, PreparedTransform, ScriptTransform,
	TransformAdapter, TransformRequest,
};
use crate::project::GuiMode;

pub(crate) struct GuiAdapter;

impl TransformAdapter for GuiAdapter {
	fn prepare(
		&self,
		input: &mut ResolvedInput,
		request: &TransformRequest<'_>,
		reviewed: ReviewedInputs,
	) -> Result<Vec<PreparedTransform>, MergeError> {
		let config = &request.project.gui;
		match config.mode {
			None => return Ok(Vec::new()),
			Some(GuiMode::Decisions) => {}
			Some(mode) => {
				return Err(invalid(format!(
					"GUI mode {mode:?} is not implemented; use `decisions`"
				)));
			}
		}
		let mut guard = reviewed.source_guard;
		let sources = PlaysetText::read(input, &mut guard)?;
		let catalog = FeatureCatalog::new(GuiSources {
			interface: &sources.interface,
			custom_gui: &sources.custom_gui,
			scripted_effects: &sources.effects,
			scripted_triggers: &sources.triggers,
		});
		let context = RouteContext {
			geography: sources.geography.clone(),
			localisation: sources.localisation.clone(),
			custom_localisation: sources.custom_localisation.clone(),
			overrides: GuiOverrides::from_config(config),
		};

		// Each action belongs to the mod whose custom GUI definition wins.
		let mut by_mod: BTreeMap<String, Vec<_>> = BTreeMap::new();
		let mut unextracted: BTreeMap<String, Vec<String>> = BTreeMap::new();
		for (name, result) in catalog.actions() {
			match result {
				Ok(feature) => {
					if let Some(owner) = sources.owners.get(&feature.definition_path) {
						by_mod.entry(owner.clone()).or_default().push(feature);
					}
				}
				Err(reason) => {
					let owner = sources
						.button_owners
						.get(&name)
						.cloned()
						.unwrap_or_default();
					unextracted
						.entry(owner)
						.or_default()
						.push(format!("kept in GUI: {name}: {reason}"));
				}
			}
		}

		let mut groups = Vec::new();
		// Playset order, never sorted for determinism.
		for candidate in &input.mods {
			let mod_id = &candidate.mod_id;
			let features = by_mod.remove(mod_id).unwrap_or_default();
			let mut evidence = unextracted.remove(mod_id).unwrap_or_default();
			let own_paths = sources
				.owners
				.iter()
				.filter(|(_, owner)| *owner == mod_id)
				.map(|(path, _)| path.clone())
				.collect::<BTreeSet<_>>();
			if features.is_empty() {
				if !evidence.is_empty() {
					groups.push(PreparedTransform {
						id: format!("eu4.gui.{mod_id}"),
						paths: own_paths,
						evidence,
						..Default::default()
					});
				}
				continue;
			}
			let name = candidate
				.descriptor
				.as_ref()
				.map(|descriptor| descriptor.name.clone())
				.filter(|name| !name.is_empty())
				.or_else(|| candidate.entry.display_name.clone())
				.unwrap_or_else(|| mod_id.clone());
			let plan = super::decision::decision_route(
				&catalog,
				&features,
				SourceMod {
					id: mod_id,
					name: &name,
				},
				&context,
			);
			evidence.extend(
				plan.rejected.iter().map(|(button, reasons)| {
					format!("kept in GUI: {button}: {}", reasons.join("; "))
				}),
			);
			let Some(decision) = plan.decisions.into_iter().next() else {
				groups.push(PreparedTransform {
					id: format!("eu4.gui.{mod_id}"),
					paths: own_paths,
					evidence,
					..Default::default()
				});
				continue;
			};
			groups.push(migration(
				request,
				&sources,
				mod_id,
				decision,
				own_paths,
				evidence,
				guard.clone(),
			)?);
		}
		Ok(groups)
	}
}

/// One mod's connected migration output.
#[allow(clippy::too_many_arguments)]
fn migration(
	request: &TransformRequest<'_>,
	sources: &PlaysetText,
	mod_id: &str,
	decision: GeneratedDecision,
	own_paths: BTreeSet<GamePathBuf>,
	mut evidence: Vec<String>,
	source_guard: SourceGuard,
) -> Result<PreparedTransform, MergeError> {
	let hidden = decision.actions.iter().cloned().collect::<BTreeSet<_>>();
	let mut overlays = Vec::new();
	let mut paths = BTreeSet::new();
	let mut identity = blake3::Hasher::new();
	identity.update(b"eu4.gui.decisions\n");
	for path in &own_paths {
		let Some(parsed) = sources.frozen.get(path) else {
			continue;
		};
		let Some(hiding) = hide(&parsed.ast, &hidden) else {
			continue;
		};
		let bytes =
			emit_clausewitz_statements_with_options(&hiding.statements, request.emit_options)
				.map_err(|error| invalid(error.to_string()))?
				.into_bytes();
		identity.update(path.as_str().as_bytes());
		identity.update(&bytes);
		overlays.push(ScriptTransform::new(parsed.clone(), hiding, bytes));
		paths.insert(path.clone());
	}
	let mut generated = Vec::new();
	for (path, text) in decision.scripts.iter().chain(&decision.localisation) {
		identity.update(path.as_bytes());
		identity.update(text.as_bytes());
		let path = GamePathBuf::parse(path).map_err(|error| {
			invalid(format!(
				"generated path `{path}` is not a game path: {error}"
			))
		})?;
		generated.push(GeneratedArtifact {
			path,
			bytes: Arc::from(text.as_bytes()),
		});
	}
	evidence.push(format!(
		"GUI actions migrated to decision `{}`: {}",
		decision.id,
		decision.actions.join(", ")
	));
	if !decision.approximate_visibility.is_empty() {
		evidence.push(format!(
			"decision `{}` is shown without an exact test for: {} (add a [[gui.visibility]] override)",
			decision.id,
			decision.approximate_visibility.join(", ")
		));
	}
	evidence.extend(
		decision
			.text_problems
			.iter()
			.map(|problem| format!("decision `{}` text: {problem}", decision.id)),
	);
	Ok(PreparedTransform {
		id: format!("eu4.gui.{mod_id}"),
		identity: identity.finalize().to_hex().to_string(),
		paths,
		findings: Vec::new(),
		evidence,
		entity_transform: None,
		validator: Some(Arc::new(MigrationCheck {
			hidden: hidden.into_iter().collect(),
			generated: generated
				.iter()
				.map(|artifact| artifact.path.clone())
				.collect(),
		})),
		source_guard,
		overlays,
		generated,
	})
}

/// `custom_*` definitions of `buttons` with `potential = { always = no }`,
/// or `None` when the document defines none of them.
fn hide(ast: &AstFile, buttons: &BTreeSet<String>) -> Option<AstFile> {
	let mut changed = false;
	let statements = ast
		.statements
		.iter()
		.map(|statement| match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value: AstValue::Block { items, span: inner },
				span,
			} if key.starts_with("custom_")
				&& scalar(items, "name").is_some_and(|name| buttons.contains(&name)) =>
			{
				changed = true;
				let mut hidden = items
					.iter()
					.filter(
						|item| !matches!(item, AstStatement::Assignment { key, .. } if key == "potential"),
					)
					.cloned()
					.collect::<Vec<_>>();
				let at = hidden
					.iter()
					.position(
						|item| matches!(item, AstStatement::Assignment { key, .. } if key == "name"),
					)
					.map_or(0, |index| index + 1);
				hidden.insert(
					at,
					assign("potential", block(vec![assign("always", identifier("no"))])),
				);
				AstStatement::Assignment {
					key: key.clone(),
					key_span: key_span.clone(),
					value: AstValue::Block {
						items: hidden,
						span: inner.clone(),
					},
					span: span.clone(),
				}
			}
			other => other.clone(),
		})
		.collect();
	changed.then(|| AstFile {
		path: ast.path.clone(),
		statements,
	})
}

/// Audits the emitted group: every generated file is present and every
/// migrated button's effective definition is hidden, so a lower-precedence
/// definition cannot bring the old entry back.
#[derive(Debug)]
struct MigrationCheck {
	hidden: Vec<String>,
	generated: Vec<GamePathBuf>,
}

impl OutputValidation for MigrationCheck {
	fn validate_emitted(&self, output: &EmittedOutput) -> Vec<String> {
		let mut problems = Vec::new();
		let emitted = output
			.scripts
			.iter()
			.map(|(path, _)| path)
			.chain(output.localisation.iter().map(|(path, _)| path))
			.chain(output.resources.iter().map(|(path, _)| path))
			.collect::<BTreeSet<_>>();
		for path in &self.generated {
			if !emitted.contains(path) {
				problems.push(format!("generated `{path}` is not in the output"));
			}
		}
		for button in &self.hidden {
			let definitions = output
				.scripts
				.iter()
				.flat_map(|(_, ast)| &ast.statements)
				.filter_map(|statement| match statement {
					AstStatement::Assignment {
						key,
						value: AstValue::Block { items, .. },
						..
					} if key.starts_with("custom_")
						&& scalar(items, "name").as_deref() == Some(button.as_str()) =>
					{
						Some(items)
					}
					_ => None,
				})
				.collect::<Vec<_>>();
			if definitions.is_empty() {
				problems.push(format!(
					"migrated button `{button}` has no definition in the output"
				));
			}
			for items in definitions {
				let hidden = items.iter().any(|item| match item {
					AstStatement::Assignment {
						key,
						value: AstValue::Block { items, .. },
						..
					} if key == "potential" => {
						matches!(items.as_slice(), [AstStatement::Assignment { key, value: AstValue::Scalar { value, .. }, .. }] if key == "always" && value.as_text() == "no")
					}
					_ => false,
				});
				if !hidden {
					problems.push(format!(
						"migrated button `{button}` is still visible in the output"
					));
				}
			}
		}
		problems
	}
}

/// The effective text of the playset the GUI route reads.
struct PlaysetText {
	interface: Vec<AstFile>,
	custom_gui: Vec<AstFile>,
	/// Frozen custom GUI documents the migration may overlay, by path.
	frozen: BTreeMap<GamePathBuf, Arc<ParsedScriptFile>>,
	/// Mod whose custom GUI document wins each path.
	owners: BTreeMap<GamePathBuf, String>,
	/// Mod defining each custom GUI name, for reports.
	button_owners: BTreeMap<String, String>,
	effects: Vec<ScriptSource>,
	triggers: Vec<ScriptSource>,
	localisation: EffectiveLocalisation,
	custom_localisation: CustomLocalisation,
	geography: Geography,
}

impl PlaysetText {
	fn read(input: &ResolvedInput, guard: &mut SourceGuard) -> Result<Self, MergeError> {
		let mut text = Self {
			interface: Vec::new(),
			custom_gui: Vec::new(),
			frozen: BTreeMap::new(),
			owners: BTreeMap::new(),
			button_owners: BTreeMap::new(),
			effects: Vec::new(),
			triggers: Vec::new(),
			localisation: EffectiveLocalisation::default(),
			custom_localisation: CustomLocalisation::default(),
			geography: Geography::default(),
		};
		let mut custom_localisation = Vec::new();
		let mut localisation = Vec::new();
		let mut superregion = None;
		let mut region = None;
		for (path, contributors) in &input.file_inventory {
			let Some(winner) = contributors.last() else {
				continue;
			};
			let name = path.as_str();
			if name.starts_with("interface/") && name.ends_with(".gui") {
				// Every mod's own layout, where its buttons are placed.
				for contributor in contributors.iter().filter(|item| !item.is_base_game) {
					let bytes = read(input, contributor, guard)?;
					text.interface.push(parse(path, &bytes));
				}
			} else if name.starts_with("common/custom_gui/") && name.ends_with(".txt") {
				let bytes = read(input, winner, guard)?;
				let document = parse(path, &bytes);
				for statement in &document.statements {
					if let AstStatement::Assignment {
						value: AstValue::Block { items, .. },
						..
					} = statement && let Some(name) = scalar(items, "name")
					{
						text.button_owners.insert(name, winner.mod_id.clone());
					}
				}
				text.custom_gui.push(document);
				if !winner.is_base_game {
					text.owners.insert(path.clone(), winner.mod_id.clone());
					if let Ok(parsed) = input.script_cache.load(winner) {
						text.frozen.insert(path.clone(), parsed);
					}
				}
			} else if name.starts_with("common/scripted_effects/") && name.ends_with(".txt") {
				text.effects
					.push(source(path, &read(input, winner, guard)?));
			} else if name.starts_with("common/scripted_triggers/") && name.ends_with(".txt") {
				text.triggers
					.push(source(path, &read(input, winner, guard)?));
			} else if name.starts_with("customizable_localization/") && name.ends_with(".txt") {
				custom_localisation.push(parse(path, &read(input, winner, guard)?));
			} else if name.starts_with("localisation/") && name.ends_with(".yml") {
				for contributor in contributors {
					localisation.push((contributor.precedence, read(input, contributor, guard)?));
				}
			} else if name == "map/superregion.txt" {
				superregion = Some(parse(path, &read(input, winner, guard)?));
			} else if name == "map/region.txt" {
				region = Some(parse(path, &read(input, winner, guard)?));
			}
		}
		// Highest precedence first: the first definition of a key wins.
		localisation.sort_by_key(|(precedence, _)| std::cmp::Reverse(*precedence));
		for (_, bytes) in &localisation {
			text.localisation.add_file(bytes);
		}
		text.custom_localisation = CustomLocalisation::new(&custom_localisation);
		if let (Some(superregion), Some(region)) = (superregion, region) {
			text.geography = Geography::from_map(&superregion, &region);
		}
		Ok(text)
	}
}

/// The bytes the loader reads for a contributor; non-base files are bound.
fn read(
	input: &ResolvedInput,
	contributor: &ResolvedInputContributor,
	guard: &mut SourceGuard,
) -> Result<Vec<u8>, MergeError> {
	let bytes = match input
		.script_cache
		.overlay_bytes(&contributor.mod_id, &contributor.relative_path)
	{
		Some(bytes) => bytes.to_vec(),
		None => fs::read(contributor.absolute_path())?,
	};
	if !contributor.is_base_game {
		guard.bind(contributor.absolute_path(), &bytes)?;
	}
	Ok(bytes)
}

fn parse(path: &GamePath, bytes: &[u8]) -> AstFile {
	parse_clausewitz_content(path, &decode_paradox_bytes(bytes)).ast
}

fn source(path: &GamePath, bytes: &[u8]) -> ScriptSource {
	ScriptSource {
		path: path.to_owned(),
		text: decode_paradox_bytes(bytes).into_owned(),
	}
}

fn scalar(items: &[AstStatement], key: &str) -> Option<String> {
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key: found,
			value: AstValue::Scalar { value, .. },
			..
		} if found == key => Some(value.as_text()),
		_ => None,
	})
}

fn invalid(message: impl Into<String>) -> MergeError {
	MergeError::Validation {
		subject: None,
		message: message.into(),
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeSet;

	use super::*;
	use crate::game::eu4::script::emit::EmitOptions;
	use crate::input::config::Config;
	use crate::input::request::InputRequest;
	use crate::merge::dag::IgnoreReplacePath;
	use crate::merge::output::materialize::{
		MaterializeOutput, MergeMaterializeOptions, materialize_with_adaptations,
	};
	use crate::merge::transform::TransformPlan;
	use crate::model::{
		MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy, MergePlanTarget,
		ModCandidate,
	};
	use crate::playset::descriptor::ModDescriptor;
	use crate::playset::{Playset, PlaysetEntry};
	use crate::project::{GuiConfig, Project};

	const FILES: &[(&str, &str)] = &[
		(
			"interface/countryreligionview.gui",
			r#"guiTypes = { windowType = { name = "countryreligionview"
	windowType = { name = "rce_purity_window" scripted = yes
		guiButtonType = { name = "rce_purity_knowledge_button" scripted = yes }
		guiButtonType = { name = "rce_purity_wind_button" scripted = yes }
	} } }
"#,
		),
		(
			"common/custom_gui/rce_purity.txt",
			r#"custom_window = { name = rce_purity_window potential = { ai = no } }
custom_button = {
	name = rce_purity_knowledge_button
	tooltip = rce_purity_knowledge_tt
	potential = { has_country_flag = purity }
	effect = { add_adm_power = -20 }
}
custom_button = {
	name = rce_purity_wind_button
	tooltip = rce_purity_wind_tt
	effect = { add_dip_power = -10 }
}
"#,
		),
		(
			"localisation/rce_l_english.yml",
			"\u{feff}l_english:\n rce_purity_knowledge_tt:0 \"Offer\"\n rce_purity_wind_tt:0 \"Wind\"\n",
		),
	];

	fn input(root: &std::path::Path) -> ResolvedInput {
		let mod_root = root.join("rce");
		let mut file_inventory = BTreeMap::new();
		let mut script_cache = crate::input::InputScriptCache::default();
		for (path, text) in FILES {
			let relative = crate::model::GamePathBuf::parse(path).expect("test game path");
			let absolute = relative.to_path(&mod_root);
			fs::create_dir_all(absolute.parent().unwrap()).unwrap();
			fs::write(&absolute, text).unwrap();
			if path.ends_with(".txt") || path.ends_with(".gui") {
				let parsed = crate::game::eu4::script::parse_script_bytes_cached(
					"rce",
					&mod_root,
					&relative,
					text.as_bytes(),
				);
				script_cache.insert_overlay(parsed, text.as_bytes().to_vec());
			}
			file_inventory.insert(
				relative.clone(),
				vec![ResolvedInputContributor {
					mod_id: "rce".into(),
					root_path: mod_root.clone(),
					relative_path: relative,
					precedence: 1,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint: None,
					mod_hash: None,
				}],
			);
		}
		ResolvedInput {
			playlist_path: root.join("playlist.json"),
			playlist: Playset::default(),
			game_version: None,
			mods: vec![ModCandidate {
				entry: PlaysetEntry::default(),
				mod_id: "rce".into(),
				root_path: Some(mod_root),
				descriptor_path: None,
				descriptor: Some(ModDescriptor {
					name: "Religion Compatibility Expanded".into(),
					..Default::default()
				}),
				workshop_identity: None,
				descriptor_error: None,
				files: Vec::new(),
			}],
			installed_base_snapshot: None,
			cache_game_version: None,
			mod_snapshots: Vec::new(),
			script_cache,
			file_inventory,
			verified_absent_base_paths: BTreeSet::new(),
			requested_retained_paths: None,
			effective_retained_paths: None,
		}
	}

	fn transforms(input: &mut ResolvedInput, project: &Project) -> TransformPlan {
		let request = TransformRequest {
			project,
			dep_overrides: &[],
			ignore_replace_path: &IgnoreReplacePath::None,
			emit_options: &EmitOptions::default(),
			duplicate_definitions: None,
			reference_inventory_complete: true,
		};
		let mut plan = TransformPlan::default();
		for prepared in GuiAdapter
			.prepare(input, &request, ReviewedInputs::default())
			.unwrap()
		{
			plan.push(input, prepared).unwrap();
		}
		plan
	}

	fn decisions_project() -> Project {
		Project {
			gui: GuiConfig {
				mode: Some(GuiMode::Decisions),
				..Default::default()
			},
			..Default::default()
		}
	}

	fn copy_plan(input: &ResolvedInput) -> MergePlanResult {
		MergePlanResult {
			paths: input
				.file_inventory
				.iter()
				.map(|(path, contributors)| {
					let contributor = &contributors[0];
					let planned = MergePlanContributor {
						mod_id: contributor.mod_id.clone(),
						source_path: contributor
							.absolute_path()
							.to_string_lossy()
							.replace('\\', "/"),
						precedence: contributor.precedence,
						is_base_game: false,
					};
					MergePlanEntry {
						target: MergePlanTarget::File { path: path.clone() },
						strategy: MergePlanStrategy::CopyThrough,
						contributors: vec![planned.clone()],
						winner: Some(planned),
						notes: Vec::new(),
					}
				})
				.collect(),
			..Default::default()
		}
	}

	#[test]
	fn decisions_mode_writes_the_decision_and_hides_the_buttons_together() {
		let temp = tempfile::TempDir::new().unwrap();
		let mut input = input(temp.path());
		let mut plan = transforms(&mut input, &decisions_project());
		let mut merge = copy_plan(&input);
		// As analysis does: register generated files, then annotate units.
		plan.register_generated(&input.file_inventory, &mut merge);
		plan.annotate(&mut merge);
		let generated = merge
			.paths
			.iter()
			.filter(|entry| entry.strategy == MergePlanStrategy::Generated)
			.map(|entry| entry.output_path().to_owned())
			.collect::<Vec<_>>();
		assert!(
			generated
				.iter()
				.any(|path| path.as_str().starts_with("decisions/"))
		);
		assert!(
			generated
				.iter()
				.any(|path| path.as_str().starts_with("events/"))
		);
		assert!(
			generated
				.iter()
				.any(|path| path.as_str().starts_with("localisation/"))
		);
		let out = temp.path().join("out");
		let result = materialize_with_adaptations(
			InputRequest::from_playset_path(temp.path().join("playlist.json"), Config::default()),
			MaterializeOutput {
				artifacts_dir: &out,
				prior_dir: None,
				target_dir: &out,
			},
			MergeMaterializeOptions::default(),
			Ok(input),
			merge,
			None,
			&plan,
		)
		.unwrap();
		for path in &generated {
			assert!(path.to_path(&out).is_file(), "{path} not written");
		}
		let custom_gui = fs::read_to_string(out.join("common/custom_gui/rce_purity.txt")).unwrap();
		assert_eq!(
			custom_gui.matches("always = no").count(),
			2,
			"both migrated buttons are hidden\n{custom_gui}"
		);
		assert!(custom_gui.contains("add_adm_power = -20"), "{custom_gui}");
		let source =
			fs::read_to_string(temp.path().join("rce/common/custom_gui/rce_purity.txt")).unwrap();
		assert_eq!(source, FILES[1].1, "source mods stay read-only");
		let decision_path = generated
			.iter()
			.find(|path| path.as_str().starts_with("decisions/"))
			.unwrap();
		let decision = fs::read_to_string(decision_path.to_path(&out)).unwrap();
		assert!(
			decision.contains("ai = no") && decision.contains("factor = 0"),
			"{decision}"
		);
		let units = result.review.units();
		assert!(
			units
				.iter()
				.filter(|unit| generated.contains(&unit.path))
				.all(|unit| unit.disposition == crate::merge::MergeDisposition::Safe)
		);
		let gui_unit = units
			.iter()
			.find(|unit| unit.path.as_str() == "common/custom_gui/rce_purity.txt")
			.unwrap();
		assert!(
			gui_unit
				.notes
				.iter()
				.any(|note| note.contains("GUI actions migrated to decision")),
			"{:?}",
			gui_unit.notes
		);
	}

	#[test]
	fn without_a_mode_the_merge_is_unchanged() {
		let temp = tempfile::TempDir::new().unwrap();
		let mut input = input(temp.path());
		let mut plan = transforms(&mut input, &Project::default());
		let mut merge = copy_plan(&input);
		let before = merge.paths.len();
		plan.register_generated(&input.file_inventory, &mut merge);
		assert_eq!(merge.paths.len(), before);
		assert_eq!(plan.cache_identity("base"), "base");
	}

	#[test]
	fn the_migration_binds_the_mod_files_it_read() {
		let temp = tempfile::TempDir::new().unwrap();
		let mut input = input(temp.path());
		let plan = transforms(&mut input, &decisions_project());
		plan.source_guard.validate().unwrap();
		fs::write(
			temp.path().join("rce/localisation/rce_l_english.yml"),
			"\u{feff}l_english:\n rce_purity_knowledge_tt:0 \"Changed\"\n",
		)
		.unwrap();
		assert!(plan.source_guard.validate().is_err());
	}

	#[test]
	fn the_output_check_rejects_a_visible_old_entry_or_missing_files() {
		let check = MigrationCheck {
			hidden: vec!["rce_purity_wind_button".into()],
			generated: vec![
				crate::model::GamePathBuf::parse("decisions/foch_gui_x.txt")
					.expect("test game path"),
			],
		};
		let mut hidden = EmittedOutput::default();
		hidden.push(
			&crate::model::GamePathBuf::parse("common/custom_gui/rce_purity.txt")
				.expect("test game path"),
			b"custom_button = { name = rce_purity_wind_button potential = { always = no } }",
		);
		hidden.push(
			&crate::model::GamePathBuf::parse("decisions/foch_gui_x.txt").expect("test game path"),
			b"country_decisions = { }",
		);
		assert!(check.validate_emitted(&hidden).is_empty());

		// A lower-precedence definition still showing the button, and no decision.
		let mut visible = EmittedOutput::default();
		visible.push(
			&crate::model::GamePathBuf::parse("common/custom_gui/rce_purity.txt")
				.expect("test game path"),
			b"custom_button = { name = rce_purity_wind_button potential = { always = no } }",
		);
		visible.push(
			&crate::model::GamePathBuf::parse("common/custom_gui/other.txt")
				.expect("test game path"),
			b"custom_button = { name = rce_purity_wind_button effect = { add_prestige = 1 } }",
		);
		assert_eq!(
			check.validate_emitted(&visible),
			[
				"generated `decisions/foch_gui_x.txt` is not in the output",
				"migrated button `rce_purity_wind_button` is still visible in the output",
			]
		);
		assert_eq!(
			check.validate_emitted(&EmittedOutput::default()),
			[
				"generated `decisions/foch_gui_x.txt` is not in the output",
				"migrated button `rce_purity_wind_button` has no definition in the output",
			]
		);
	}
}
