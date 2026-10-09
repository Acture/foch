use super::error::{MergeError, MergeErrorSubject};
use crate::game::eu4::content::eu4;
use crate::model::{
	GamePath, GamePathBuf, MergePlanContributor, MergePlanEntry, MergePlanResult,
	MergePlanStrategy, MergePlanTarget,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeDisposition {
	Safe,
	Copy,
	NeedsUserChoice,
	UnsupportedInput,
	EngineFailure,
	Deferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeUnitKind {
	File,
	DefinitionModule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeReviewContributor {
	pub mod_id: String,
	pub name: String,
	/// The contributor's physical files, rendered for display.
	pub source_paths: Vec<String>,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeUnitOutcome {
	/// The unit's stable key: `file:{path}` for a file, or
	/// `module:{family}/{module}` for a definition module.
	pub id: String,
	pub path: GamePathBuf,
	pub family: String,
	pub kind: MergeUnitKind,
	pub disposition: MergeDisposition,
	pub strategy: String,
	pub summary: String,
	pub output_path: Option<GamePathBuf>,
	/// Every file this unit wrote, in plan order. A unit that writes several
	/// files — an EU4 database fed by more than one directory — commits all of
	/// them or none, so this is empty exactly when `output_path` is `None`.
	pub output_paths: Vec<GamePathBuf>,
	pub contributors: Vec<MergeReviewContributor>,
	pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MergeReviewSummary {
	pub total: usize,
	pub safe: usize,
	pub copy: usize,
	pub needs_user_choice: usize,
	pub unsupported_input: usize,
	pub engine_failure: usize,
	pub deferred: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct MergeReview {
	units: Vec<MergeUnitOutcome>,
	by_id: BTreeMap<String, usize>,
	summary: MergeReviewSummary,
}

impl MergeReview {
	pub(super) fn summary(&self) -> &MergeReviewSummary {
		&self.summary
	}

	pub(super) fn units(&self) -> &[MergeUnitOutcome] {
		&self.units
	}

	pub(super) fn unit(&self, id: &str) -> Option<&MergeUnitOutcome> {
		self.by_id.get(id).map(|index| &self.units[*index])
	}
}

pub(super) struct UnitOutcomeLedger {
	units: Vec<Option<MergeUnitOutcome>>,
	by_id: BTreeMap<String, usize>,
}

impl UnitOutcomeLedger {
	pub(super) fn outcome(&self, entry: &MergePlanEntry) -> Result<&MergeUnitOutcome, MergeError> {
		let id = stable_unit_id(entry)?;
		self.by_id
			.get(&id)
			.and_then(|index| self.units[*index].as_ref())
			.ok_or_else(|| {
				invariant(
					entry.output_path(),
					"adaptation requires a resolved review unit",
				)
			})
	}

	/// Withdraw a locally resolved unit when its cross-unit prerequisites fail.
	/// Preserve original failures, while safe dependent units become reviewable.
	pub(super) fn withhold_dependency(
		&mut self,
		entry: &MergePlanEntry,
		reason: &str,
	) -> Result<(), MergeError> {
		let id = stable_unit_id(entry)?;
		let index = *self
			.by_id
			.get(&id)
			.ok_or_else(|| invariant(entry.output_path(), "unknown adaptation review unit"))?;
		let unit = self.units[index]
			.as_mut()
			.ok_or_else(|| invariant(entry.output_path(), "unresolved adaptation review unit"))?;
		if matches!(
			unit.disposition,
			MergeDisposition::Safe | MergeDisposition::Copy
		) {
			unit.disposition = MergeDisposition::NeedsUserChoice;
			unit.summary = reason.to_owned();
		}
		unit.output_path = None;
		unit.output_paths.clear();
		unit.notes.push(reason.to_owned());
		Ok(())
	}

	pub(super) fn from_plan(plan: &MergePlanResult) -> Result<Self, MergeError> {
		let mut by_id = BTreeMap::new();
		let mut output_paths = BTreeSet::new();
		let mut units = Vec::with_capacity(plan.paths.len());
		for (index, entry) in plan.paths.iter().enumerate() {
			let id = stable_unit_id(entry)?;
			if by_id.insert(id.clone(), index).is_some() {
				return Err(invariant(
					entry.output_path(),
					format!("duplicate review unit id `{id}`"),
				));
			}
			// A unit can write more than one file: an EU4 database fed by
			// several directories keeps one output per directory. Every one of
			// them must be unique across the plan, not just the primary.
			for output_path in entry.target.output_paths() {
				if !output_paths.insert(output_path) {
					return Err(invariant(
						output_path,
						format!("duplicate review output path `{output_path}`"),
					));
				}
			}
			units.push(None);
		}
		Ok(Self { units, by_id })
	}

	pub(super) fn resolve(
		&mut self,
		entry: &MergePlanEntry,
		disposition: MergeDisposition,
		summary: impl Into<String>,
		output_path: Option<GamePathBuf>,
		additional_notes: impl IntoIterator<Item = String>,
	) -> Result<(), MergeError> {
		self.resolve_written(
			entry,
			disposition,
			summary,
			output_path.into_iter().collect(),
			additional_notes,
		)
	}

	/// Record a unit against the files it actually wrote.
	///
	/// A unit can write one file per contributing directory, and a directory
	/// whose merge is a no-op against vanilla writes none, so what was written
	/// is not derivable from the plan.
	pub(super) fn resolve_written(
		&mut self,
		entry: &MergePlanEntry,
		disposition: MergeDisposition,
		summary: impl Into<String>,
		written_paths: Vec<GamePathBuf>,
		additional_notes: impl IntoIterator<Item = String>,
	) -> Result<(), MergeError> {
		let id = stable_unit_id(entry)?;
		let Some(index) = self.by_id.get(&id).copied() else {
			return Err(invariant(
				entry.output_path(),
				format!("unknown review unit `{id}`"),
			));
		};
		if self.units[index].is_some() {
			return Err(invariant(
				entry.output_path(),
				format!("review unit `{id}` resolved twice"),
			));
		}
		let (kind, family) = unit_kind_and_family(entry);
		let mut notes = entry.notes.clone();
		notes.extend(additional_notes);
		let planned_paths: Vec<&GamePath> = entry.target.output_paths();
		for path in &written_paths {
			if !planned_paths.contains(&path.as_game_path()) {
				return Err(invariant(
					entry.output_path(),
					format!(
						"review output path `{path}` is not one of this unit's planned outputs `{}`",
						planned_paths
							.iter()
							.map(|planned| planned.as_str())
							.collect::<Vec<_>>()
							.join(", ")
					),
				));
			}
		}
		let output_paths: Vec<GamePathBuf> = written_paths;
		let output_path: Option<GamePathBuf> = output_paths.first().cloned();
		self.units[index] = Some(MergeUnitOutcome {
			id,
			path: entry.output_path().to_owned(),
			family,
			kind,
			disposition,
			strategy: strategy_name(entry.strategy).to_string(),
			summary: summary.into(),
			output_paths,
			output_path,
			contributors: review_contributors(&entry.contributors),
			notes,
		});
		Ok(())
	}

	pub(super) fn finish(
		mut self,
		mod_display_names: &HashMap<String, String>,
	) -> Result<MergeReview, MergeError> {
		for unit in self.units.iter_mut().flatten() {
			for contributor in &mut unit.contributors {
				contributor.name = if contributor.is_base_game {
					"Europa Universalis IV".to_string()
				} else {
					mod_display_names
						.get(&contributor.mod_id)
						.cloned()
						.unwrap_or_else(|| contributor.mod_id.clone())
				};
			}
		}
		let mut resolved = Vec::with_capacity(self.units.len());
		for (index, unit) in self.units.into_iter().enumerate() {
			let Some(unit) = unit else {
				let id = self
					.by_id
					.iter()
					.find_map(|(id, candidate)| (*candidate == index).then_some(id.as_str()))
					.unwrap_or("<unknown>");
				return Err(invariant(
					MergeErrorSubject::Named(id.to_string()),
					format!("review unit `{id}` is still pending"),
				));
			};
			resolved.push(unit);
		}
		let summary = summarize(&resolved);
		let disposition_total = summary.safe
			+ summary.copy
			+ summary.needs_user_choice
			+ summary.unsupported_input
			+ summary.engine_failure
			+ summary.deferred;
		if summary.total != resolved.len() || disposition_total != summary.total {
			return Err(invariant(
				MergeErrorSubject::Named("review".to_string()),
				"review summary does not cover every unit",
			));
		}
		Ok(MergeReview {
			units: resolved,
			by_id: self.by_id,
			summary,
		})
	}

	pub(super) fn mark_output_pruned(
		&mut self,
		paths: &BTreeSet<GamePathBuf>,
	) -> Result<(), MergeError> {
		for path in paths {
			let Some(unit) = self
				.units
				.iter_mut()
				.flatten()
				.find(|unit| unit.output_paths.contains(path))
			else {
				return Err(invariant(
					path.as_game_path(),
					"pruned output does not match a resolved review unit",
				));
			};
			// A unit that writes several files keeps the rest: pruning one
			// directory's duplicate does not withdraw the others.
			unit.output_paths.retain(|written| written != path);
			if unit.output_path.as_ref() == Some(path) {
				unit.output_path = unit.output_paths.first().cloned();
			}
			unit.notes
				.push("cross-file semantic duplicate pruned from output".to_string());
		}
		Ok(())
	}
}

/// The unit's review id. A file id is its game path's canonical text; a
/// module id joins its family and module names, which are identifiers rather
/// than paths and only need to be present.
fn stable_unit_id(entry: &MergePlanEntry) -> Result<String, MergeError> {
	match &entry.target {
		MergePlanTarget::File { path } => Ok(format!("file:{path}")),
		MergePlanTarget::Module { id, .. } => {
			for part in [&id.family_id, &id.module_name] {
				if part.is_empty() {
					return Err(invariant(
						entry.output_path(),
						"a module review id needs a family and a module name",
					));
				}
			}
			Ok(format!("module:{}/{}", id.family_id, id.module_name))
		}
	}
}

fn unit_kind_and_family(entry: &MergePlanEntry) -> (MergeUnitKind, String) {
	match &entry.target {
		MergePlanTarget::File { path } => (
			MergeUnitKind::File,
			eu4().classify_content_family(path).map_or_else(
				|| "unclassified".to_string(),
				|descriptor| descriptor.id.as_str().to_string(),
			),
		),
		MergePlanTarget::Module { id, .. } => {
			(MergeUnitKind::DefinitionModule, id.family_id.clone())
		}
	}
}

/// One review contributor per mod, identified by mod id and base-game flag.
/// Its source list is display text: a file a plan lists twice for one mod (a
/// synthetic base and the seed it copies) is shown once.
fn review_contributors(contributors: &[MergePlanContributor]) -> Vec<MergeReviewContributor> {
	let mut output = Vec::<MergeReviewContributor>::new();
	let mut by_identity = BTreeMap::<(bool, String), usize>::new();
	for contributor in contributors {
		let identity = (contributor.is_base_game, contributor.mod_id.clone());
		if let Some(index) = by_identity.get(&identity).copied() {
			output[index].precedence = output[index].precedence.max(contributor.precedence);
			if !output[index]
				.source_paths
				.contains(&contributor.source_path)
			{
				output[index]
					.source_paths
					.push(contributor.source_path.clone());
			}
			continue;
		}
		let index = output.len();
		by_identity.insert(identity, index);
		output.push(MergeReviewContributor {
			mod_id: contributor.mod_id.clone(),
			name: contributor.mod_id.clone(),
			source_paths: vec![contributor.source_path.clone()],
			precedence: contributor.precedence,
			is_base_game: contributor.is_base_game,
		});
	}
	output.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| right.is_base_game.cmp(&left.is_base_game))
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	output
}

fn strategy_name(strategy: MergePlanStrategy) -> &'static str {
	match strategy {
		MergePlanStrategy::CopyThrough => "copy_through",
		MergePlanStrategy::LastWriterOverlay => "last_writer_overlay",
		MergePlanStrategy::StructuralMerge => "structural_merge",
		MergePlanStrategy::LocalisationMerge => "localisation_merge",
		MergePlanStrategy::ManualConflict => "manual_conflict",
		MergePlanStrategy::Generated => "generated",
	}
}

fn summarize(units: &[MergeUnitOutcome]) -> MergeReviewSummary {
	let mut summary = MergeReviewSummary {
		total: units.len(),
		..MergeReviewSummary::default()
	};
	for unit in units {
		match unit.disposition {
			MergeDisposition::Safe => summary.safe += 1,
			MergeDisposition::Copy => summary.copy += 1,
			MergeDisposition::NeedsUserChoice => summary.needs_user_choice += 1,
			MergeDisposition::UnsupportedInput => summary.unsupported_input += 1,
			MergeDisposition::EngineFailure => summary.engine_failure += 1,
			MergeDisposition::Deferred => summary.deferred += 1,
		}
	}
	summary
}

fn invariant(subject: impl Into<MergeErrorSubject>, message: impl Into<String>) -> MergeError {
	MergeError::Validation {
		subject: Some(subject.into()),
		message: message.into(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::{MergeModuleOutput, MergeModuleOutputs, MergePlanStrategies, MergeUnitId};

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn entry(path: &str, strategy: MergePlanStrategy) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::File {
				path: game_path(path),
			},
			strategy,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		}
	}

	#[test]
	fn ledger_preserves_order_and_indexes_all_six_dispositions() {
		let dispositions = [
			MergeDisposition::Safe,
			MergeDisposition::Copy,
			MergeDisposition::NeedsUserChoice,
			MergeDisposition::UnsupportedInput,
			MergeDisposition::EngineFailure,
			MergeDisposition::Deferred,
		];
		let paths = dispositions
			.iter()
			.enumerate()
			.map(|(index, _)| {
				entry(
					&format!("common/test/{index}.txt"),
					MergePlanStrategy::StructuralMerge,
				)
			})
			.collect::<Vec<_>>();
		let plan = MergePlanResult {
			paths,
			strategies: MergePlanStrategies {
				total_paths: 6,
				..Default::default()
			},
			..Default::default()
		};
		let mut ledger = UnitOutcomeLedger::from_plan(&plan).unwrap();
		for (entry, disposition) in plan.paths.iter().zip(dispositions) {
			ledger
				.resolve(
					entry,
					disposition,
					"done",
					Some(entry.output_path().to_owned()),
					[],
				)
				.unwrap();
		}
		let review = ledger.finish(&HashMap::new()).unwrap();
		assert_eq!(
			review.summary(),
			&MergeReviewSummary {
				total: 6,
				safe: 1,
				copy: 1,
				needs_user_choice: 1,
				unsupported_input: 1,
				engine_failure: 1,
				deferred: 1
			}
		);
		assert_eq!(review.units()[0].id, "file:common/test/0.txt");
		assert_eq!(
			review.unit("file:common/test/5.txt"),
			Some(&review.units()[5])
		);
	}

	#[test]
	fn module_id_and_contributors_are_stable_and_deduplicated() {
		let contributor = MergePlanContributor {
			mod_id: "mod-a".into(),
			source_path: "common/ideas/a.txt".into(),
			precedence: 2,
			is_base_game: false,
		};
		let mut second = contributor.clone();
		second.source_path = "common/ideas/b.txt".into();
		let entry = MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "ideas".into(),
					module_name: "ideas".into(),
				},
				input_paths: vec![],
				outputs: MergeModuleOutputs::new(vec![
					MergeModuleOutput::new(game_path("common/ideas/zzz_foch_ideas.txt"), None)
						.expect("output inside a namespace directory"),
				])
				.expect("one output"),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: vec![contributor, second],
			winner: None,
			notes: vec![],
		};
		let plan = MergePlanResult {
			paths: vec![entry],
			..Default::default()
		};
		let mut ledger = UnitOutcomeLedger::from_plan(&plan).unwrap();
		ledger
			.resolve(
				&plan.paths[0],
				MergeDisposition::Safe,
				"merged",
				Some(plan.paths[0].output_path().to_owned()),
				[],
			)
			.unwrap();
		let review = ledger
			.finish(&HashMap::from([("mod-a".to_string(), "Mod A".to_string())]))
			.unwrap();
		assert_eq!(review.units()[0].id, "module:ideas/ideas");
		assert_eq!(review.units()[0].contributors[0].source_paths.len(), 2);
	}

	/// A file unit's id is its game path, so a path that escapes the output
	/// or names a drive cannot even enter a plan: reading one back fails. A
	/// module id needs both of its names.
	#[test]
	fn ledger_rejects_invalid_ids_duplicate_paths_double_resolution_and_pending_finish() {
		for invalid in ["../bad.txt", r"C:\absolute\bad.txt"] {
			let json = format!(
				r#"{{"target":{{"kind":"file","path":{}}},"strategy":"copy_through","contributors":[],"winner":null}}"#,
				serde_json::to_string(invalid).expect("encode path text")
			);
			let error = serde_json::from_str::<MergePlanEntry>(&json).expect_err(invalid);
			assert!(error.to_string().contains("invalid game path"), "{error}");
		}
		let mut unnamed = entry("common/ideas/a.txt", MergePlanStrategy::StructuralMerge);
		unnamed.target = MergePlanTarget::Module {
			id: MergeUnitId {
				family_id: String::new(),
				module_name: "ideas".to_string(),
			},
			input_paths: Vec::new(),
			outputs: MergeModuleOutputs::new(vec![
				MergeModuleOutput::new(game_path("common/ideas/zzz_foch_ideas.txt"), None)
					.expect("output inside a namespace directory"),
			])
			.expect("one output"),
		};
		let unnamed = MergePlanResult {
			paths: vec![unnamed],
			..Default::default()
		};
		assert!(UnitOutcomeLedger::from_plan(&unnamed).is_err());
		let duplicate = MergePlanResult {
			paths: vec![
				entry("a.txt", MergePlanStrategy::CopyThrough),
				entry("a.txt", MergePlanStrategy::CopyThrough),
			],
			..Default::default()
		};
		assert!(UnitOutcomeLedger::from_plan(&duplicate).is_err());
		let plan = MergePlanResult {
			paths: vec![entry("a.txt", MergePlanStrategy::CopyThrough)],
			..Default::default()
		};
		let mut ledger = UnitOutcomeLedger::from_plan(&plan).unwrap();
		ledger
			.resolve(
				&plan.paths[0],
				MergeDisposition::Copy,
				"copied",
				Some(game_path("a.txt")),
				[],
			)
			.unwrap();
		assert!(
			ledger
				.resolve(
					&plan.paths[0],
					MergeDisposition::Copy,
					"copied",
					Some(game_path("a.txt")),
					[]
				)
				.is_err()
		);
		let pending = UnitOutcomeLedger::from_plan(&plan).unwrap();
		assert!(pending.finish(&HashMap::new()).is_err());

		let mut wrong_output = UnitOutcomeLedger::from_plan(&plan).unwrap();
		assert!(
			wrong_output
				.resolve(
					&plan.paths[0],
					MergeDisposition::Copy,
					"copied",
					Some(game_path("different.txt")),
					[]
				)
				.is_err()
		);
	}

	/// Review ids used to fold `\` into `/`. A unit's path is a game path now,
	/// which has no `\` spelling at all, so its id is simply the canonical
	/// text: the same id every valid path had before. Contributors still show
	/// product names.
	#[test]
	fn ids_are_the_canonical_game_path_text_and_contributors_use_product_names() {
		assert!(GamePathBuf::parse("common\\test\\one.txt").is_err());
		let base = MergePlanContributor {
			mod_id: "base:eu4".into(),
			source_path: "common\\test\\base.txt".into(),
			precedence: 0,
			is_base_game: true,
		};
		let mut low = MergePlanContributor {
			mod_id: "mod-a".into(),
			source_path: "common/test/low.txt".into(),
			precedence: 1,
			is_base_game: false,
		};
		let mut high = low.clone();
		high.source_path = "common/test/high.txt".into();
		high.precedence = 5;
		let mut planned = entry("common/test/one.txt", MergePlanStrategy::CopyThrough);
		planned.contributors = vec![base, low.clone(), high];
		low.source_path = "unused".into();
		let plan = MergePlanResult {
			paths: vec![planned],
			..Default::default()
		};
		let mut ledger = UnitOutcomeLedger::from_plan(&plan).unwrap();
		ledger
			.resolve(
				&plan.paths[0],
				MergeDisposition::Copy,
				"copied",
				Some(game_path("common/test/one.txt")),
				[],
			)
			.unwrap();
		let review = ledger
			.finish(&HashMap::from([("mod-a".into(), "Example Mod".into())]))
			.unwrap();
		let unit = &review.units()[0];
		assert_eq!(unit.id, "file:common/test/one.txt");
		assert_eq!(unit.path.as_str(), "common/test/one.txt");
		assert_eq!(unit.output_path, Some(game_path("common/test/one.txt")));
		assert_eq!(review.unit("file:common/test/one.txt"), Some(unit));
		assert_eq!(unit.family, "unclassified");
		assert_eq!(unit.strategy, "copy_through");
		assert_eq!(unit.contributors[0].name, "Europa Universalis IV");
		assert_eq!(unit.contributors[1].name, "Example Mod");
		assert_eq!(unit.contributors[1].precedence, 5);
		assert_eq!(unit.contributors[1].source_paths.len(), 2);
	}
}
