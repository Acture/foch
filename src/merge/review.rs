use super::error::MergeError;
use crate::game::eu4::content::eu4;
use crate::model::{
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy, MergePlanTarget,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path};

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
	pub source_paths: Vec<String>,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeUnitOutcome {
	pub id: String,
	pub path: String,
	pub family: String,
	pub kind: MergeUnitKind,
	pub disposition: MergeDisposition,
	pub strategy: String,
	pub summary: String,
	pub output_path: Option<String>,
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
			let output_path = normalize_path(entry.output_path());
			validate_relative_id_part(&output_path, entry.output_path())?;
			if !output_paths.insert(output_path.clone()) {
				return Err(invariant(
					entry.output_path(),
					format!("duplicate review output path `{output_path}`"),
				));
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
		output_path: Option<String>,
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
		let output_path = output_path
			.map(|path| -> Result<String, MergeError> {
				let normalized = normalize_path(&path);
				validate_relative_id_part(&normalized, entry.output_path())?;
				let planned = normalize_path(entry.output_path());
				if normalized != planned {
					return Err(invariant(
						entry.output_path(),
						format!(
							"review output path `{normalized}` does not match planned output `{planned}`"
						),
					));
				}
				Ok(normalized)
			})
			.transpose()?;
		self.units[index] = Some(MergeUnitOutcome {
			id,
			path: normalize_path(entry.output_path()),
			family,
			kind,
			disposition,
			strategy: strategy_name(entry.strategy).to_string(),
			summary: summary.into(),
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
					id,
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
				"review",
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
		paths: &BTreeSet<String>,
	) -> Result<(), MergeError> {
		for path in paths {
			let normalized = normalize_path(path);
			let Some(unit) = self
				.units
				.iter_mut()
				.flatten()
				.find(|unit| unit.output_path.as_deref() == Some(normalized.as_str()))
			else {
				return Err(invariant(
					path,
					"pruned output does not match a resolved review unit",
				));
			};
			unit.output_path = None;
			unit.notes
				.push("cross-file semantic duplicate pruned from output".to_string());
		}
		Ok(())
	}
}

fn stable_unit_id(entry: &MergePlanEntry) -> Result<String, MergeError> {
	match &entry.target {
		MergePlanTarget::File { path } => {
			let path = normalize_path(path);
			validate_relative_id_part(&path, entry.output_path())?;
			Ok(format!("file:{path}"))
		}
		MergePlanTarget::Module { id, .. } => {
			let family = normalize_path(&id.family_id);
			let module = normalize_path(&id.module_name);
			validate_relative_id_part(&family, entry.output_path())?;
			validate_relative_id_part(&module, entry.output_path())?;
			Ok(format!("module:{family}/{module}"))
		}
	}
}

fn validate_relative_id_part(value: &str, output_path: &str) -> Result<(), MergeError> {
	let path = Path::new(value);
	let bytes = value.as_bytes();
	let windows_absolute = bytes.len() >= 2
		&& bytes[0].is_ascii_alphabetic()
		&& bytes[1] == b':'
		&& (bytes.len() == 2 || bytes.get(2) == Some(&b'/'));
	if value.is_empty()
		|| windows_absolute
		|| path.is_absolute()
		|| path.components().any(|component| {
			matches!(
				component,
				Component::CurDir
					| Component::ParentDir
					| Component::RootDir
					| Component::Prefix(_)
			)
		}) {
		return Err(invariant(
			output_path,
			format!("invalid review unit id component `{value}`"),
		));
	}
	Ok(())
}

fn unit_kind_and_family(entry: &MergePlanEntry) -> (MergeUnitKind, String) {
	match &entry.target {
		MergePlanTarget::File { path } => (
			MergeUnitKind::File,
			eu4().classify_content_family(Path::new(path)).map_or_else(
				|| "unclassified".to_string(),
				|descriptor| descriptor.id.as_str().to_string(),
			),
		),
		MergePlanTarget::Module { id, .. } => {
			(MergeUnitKind::DefinitionModule, id.family_id.clone())
		}
	}
}

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

fn normalize_path(path: &str) -> String {
	path.replace('\\', "/")
}

fn strategy_name(strategy: MergePlanStrategy) -> &'static str {
	match strategy {
		MergePlanStrategy::CopyThrough => "copy_through",
		MergePlanStrategy::LastWriterOverlay => "last_writer_overlay",
		MergePlanStrategy::StructuralMerge => "structural_merge",
		MergePlanStrategy::LocalisationMerge => "localisation_merge",
		MergePlanStrategy::ManualConflict => "manual_conflict",
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

fn invariant(path: impl Into<String>, message: impl Into<String>) -> MergeError {
	MergeError::Validation {
		path: Some(path.into()),
		message: message.into(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::{MergePlanStrategies, MergeUnitId};

	fn entry(path: &str, strategy: MergePlanStrategy) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::File { path: path.into() },
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
					Some(entry.output_path().into()),
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
				output_path: "common/ideas/zzz_foch_ideas.txt".into(),
				replace_prefix: None,
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
				Some(plan.paths[0].output_path().into()),
				[],
			)
			.unwrap();
		let review = ledger
			.finish(&HashMap::from([("mod-a".to_string(), "Mod A".to_string())]))
			.unwrap();
		assert_eq!(review.units()[0].id, "module:ideas/ideas");
		assert_eq!(review.units()[0].contributors[0].source_paths.len(), 2);
	}

	#[test]
	fn ledger_rejects_invalid_ids_duplicate_paths_double_resolution_and_pending_finish() {
		let invalid = MergePlanResult {
			paths: vec![entry("../bad.txt", MergePlanStrategy::CopyThrough)],
			..Default::default()
		};
		assert!(UnitOutcomeLedger::from_plan(&invalid).is_err());
		let windows_absolute = MergePlanResult {
			paths: vec![entry(
				"C:\\absolute\\bad.txt",
				MergePlanStrategy::CopyThrough,
			)],
			..Default::default()
		};
		assert!(UnitOutcomeLedger::from_plan(&windows_absolute).is_err());
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
				Some("a.txt".into()),
				[],
			)
			.unwrap();
		assert!(
			ledger
				.resolve(
					&plan.paths[0],
					MergeDisposition::Copy,
					"copied",
					Some("a.txt".into()),
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
					Some("different.txt".into()),
					[]
				)
				.is_err()
		);
	}

	#[test]
	fn ids_normalize_windows_separators_and_contributors_use_product_names() {
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
		let mut planned = entry("common\\test\\one.txt", MergePlanStrategy::CopyThrough);
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
				Some("common/test/one.txt".into()),
				[],
			)
			.unwrap();
		let review = ledger
			.finish(&HashMap::from([("mod-a".into(), "Example Mod".into())]))
			.unwrap();
		let unit = &review.units()[0];
		assert_eq!(unit.id, "file:common/test/one.txt");
		assert_eq!(unit.path, "common/test/one.txt");
		assert_eq!(unit.family, "unclassified");
		assert_eq!(unit.strategy, "copy_through");
		assert_eq!(unit.contributors[0].name, "Europa Universalis IV");
		assert_eq!(unit.contributors[1].name, "Example Mod");
		assert_eq!(unit.contributors[1].precedence, 5);
		assert_eq!(unit.contributors[1].source_paths.len(), 2);
	}
}
