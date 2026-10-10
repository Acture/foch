//! Verified culture correspondence shared by structural detection and reference adaptation.

use crate::model::GamePath;
use std::collections::{BTreeMap, BTreeSet};

use super::{CultureFinding, CultureIndex, CultureRenameMap};
use crate::game::eu4::content::{ContentFamilyPathMatcher, eu4};
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue};
use crate::merge::kernel::SemanticKey;
use crate::merge::transform::tree::EntityTransform;

#[derive(Clone, Debug, Default)]
pub(crate) struct CultureCorrespondence {
	renames: BTreeMap<String, String>,
	identities: BTreeMap<String, String>,
	groups: BTreeSet<String>,
	pub(crate) group_moves: Vec<CultureGroupMove>,
}

#[derive(Clone, Debug)]
pub(crate) struct CultureGroupMove {
	pub old: String,
	pub new: String,
	pub from: String,
	pub to: String,
	pub inherited_fields: Vec<(String, AstValue)>,
}

pub(crate) struct CultureRevision<'a> {
	pub parent: &'a CultureIndex,
	pub revision: &'a CultureIndex,
	pub reviewed: &'a BTreeMap<String, String>,
}

impl CultureCorrespondence {
	pub(crate) fn applies_to(path: &GamePath) -> bool {
		eu4()
			.classify_content_family(path)
			.is_some_and(|descriptor| {
				matches!(
					descriptor.matcher,
					ContentFamilyPathMatcher::Prefix(prefix) if prefix.as_str() == "common/cultures"
				)
			})
	}

	pub(crate) fn infer(
		before: &CultureIndex,
		revisions: &[CultureIndex],
	) -> Result<Self, Vec<CultureFinding>> {
		let reviewed = BTreeMap::new();
		Self::infer_observations(
			before,
			&revisions
				.iter()
				.map(|revision| CultureRevision {
					parent: before,
					revision,
					reviewed: &reviewed,
				})
				.collect::<Vec<_>>(),
		)
	}

	/// Each pair is an effective dependency parent and its authored child view.
	pub(crate) fn infer_observations(
		before: &CultureIndex,
		observations: &[CultureRevision<'_>],
	) -> Result<Self, Vec<CultureFinding>> {
		let mut result = Self::default();
		let mut findings = Vec::new();
		for index in std::iter::once(before).chain(
			observations
				.iter()
				.flat_map(|observation| [observation.parent, observation.revision]),
		) {
			result.groups.extend(index.groups.keys().cloned());
			for id in index.definitions.keys() {
				result.identities.insert(id.clone(), id.clone());
			}
		}
		for observation in observations {
			let parent = observation.parent;
			let after = observation.revision;
			CultureRenameMap::checked(parent, after, observation.reviewed)?;
			for (old, new) in observation.reviewed {
				record_rename(&mut result.renames, &mut findings, old, new);
			}
			for (old, definition) in &parent.definitions {
				if after.definitions.contains_key(old) || observation.reviewed.contains_key(old) {
					continue;
				}
				let candidates = after
					.definitions
					.iter()
					.filter(|(new, candidate)| {
						!parent.definitions.contains_key(*new)
							&& !observation.reviewed.values().any(|target| target == *new)
							&& same_value(&definition.body, &candidate.body)
					})
					.map(|(new, _)| new)
					.collect::<Vec<_>>();
				match candidates.as_slice() {
					[] => {}
					[new] => {
						record_rename(&mut result.renames, &mut findings, old, new);
					}
					_ => findings.push(ambiguous(format!(
						"culture `{old}` has indistinguishable rename candidates: {candidates:?}"
					))),
				}
			}
		}
		// Validate the combined mapping once, including fusion/collision/endpoint rules.
		if result
			.renames
			.values()
			.any(|target| result.renames.contains_key(target))
		{
			findings.push(CultureFinding {
				code: "unsupported_culture_rename_chain",
				message: "sequential culture rename chains require review before composing reference adaptation".into(),
				location: None,
			});
			return Err(findings);
		}
		let mut origins = before.clone();
		for source in result.renames.keys() {
			if let Some(definition) = observations
				.iter()
				.find_map(|observation| observation.parent.definitions.get(source))
			{
				origins
					.definitions
					.entry(source.clone())
					.or_insert_with(|| definition.clone());
			}
		}
		let mut after = origins.clone();
		for (old, new) in &result.renames {
			after.definitions.remove(old);
			if let Some(definition) = observations
				.iter()
				.find_map(|observation| observation.revision.definitions.get(new))
			{
				after.definitions.insert(new.clone(), definition.clone());
			}
			if observations.iter().any(|observation| {
				let index = observation.revision;
				index.definitions.contains_key(old) && index.definitions.contains_key(new)
			}) {
				findings.push(ambiguous(format!(
					"culture `{old}` and proposed rename `{new}` coexist in a contributor"
				)));
			}
			result.identities.insert(new.clone(), old.clone());
		}
		if let Err(invalid) = CultureRenameMap::checked(&origins, &after, &result.renames) {
			findings.extend(invalid);
		}
		for observation in observations {
			let parent = observation.parent;
			let revision = observation.revision;
			for (old, original) in &parent.definitions {
				let new = result.renames.get(old).unwrap_or(old);
				if let Some(changed) = revision
					.definitions
					.get(old)
					.or_else(|| revision.definitions.get(new))
					&& changed.group != original.group
				{
					result.group_moves.push(CultureGroupMove {
						old: old.clone(),
						new: new.clone(),
						from: original.group.clone(),
						to: changed.group.clone(),
						inherited_fields: parent
							.group_fields
							.get(&original.group)
							.cloned()
							.unwrap_or_default(),
					});
				}
			}
		}
		if findings.is_empty() {
			Ok(result)
		} else {
			Err(findings)
		}
	}

	pub(crate) fn from_files(
		base: &AstFile,
		revisions: &[&AstFile],
	) -> Result<Option<Self>, Vec<CultureFinding>> {
		if !Self::applies_to(&base.path) {
			return Ok(None);
		}
		let before = CultureIndex::from_documents(&[(base.path.clone(), base.clone())])?;
		let after = revisions
			.iter()
			.map(|file| CultureIndex::from_documents(&[(base.path.clone(), (*file).clone())]))
			.collect::<Result<Vec<_>, _>>()?;
		Self::infer(&before, &after).map(Some)
	}

	pub(crate) fn renames(&self) -> &BTreeMap<String, String> {
		&self.renames
	}

	pub(crate) fn validate_catalog(
		&self,
		catalog: &CultureIndex,
	) -> Result<(), Vec<CultureFinding>> {
		let mut identities = BTreeMap::new();
		for label in catalog.definitions.keys() {
			let identity = self.identities.get(label).unwrap_or(label);
			if let Some(previous) = identities.insert(identity, label) {
				return Err(vec![ambiguous(format!(
					"cultures `{previous}` and `{label}` share canonical identity eu4.culture:{identity}; correspondence needs review"
				))]);
			}
		}
		Ok(())
	}

	pub(crate) fn identity(&self, ancestors: &[String], key: &str) -> Option<&str> {
		let [group] = ancestors else {
			return None;
		};
		self.groups
			.contains(group)
			.then(|| self.identities.get(key))
			.flatten()
			.map(String::as_str)
	}
}

impl EntityTransform for CultureCorrespondence {
	fn applies_to(&self, path: &GamePath) -> bool {
		Self::applies_to(path)
	}

	fn entity_identity(
		&self,
		ancestors: &[String],
		key: &str,
		value: &AstValue,
	) -> Option<SemanticKey> {
		if !matches!(value, AstValue::Block { .. }) {
			return None;
		}
		self.identity(ancestors, key)
			.map(|id| SemanticKey::new("eu4.culture", id))
	}

	fn validate(&self, file: &AstFile) -> Result<(), String> {
		let catalog = CultureIndex::from_documents(&[(file.path.clone(), file.clone())])
			.map_err(|findings| format!("invalid culture hierarchy: {findings:?}"))?;
		self.validate_catalog(&catalog)
			.map_err(|findings| format!("culture correspondence needs review: {findings:?}"))
	}
}

fn ambiguous(message: String) -> CultureFinding {
	CultureFinding {
		code: "ambiguous_culture_correspondence",
		message,
		location: None,
	}
}

fn record_rename(
	renames: &mut BTreeMap<String, String>,
	findings: &mut Vec<CultureFinding>,
	old: &str,
	new: &str,
) {
	if let Some(previous) = renames.insert(old.to_owned(), new.to_owned())
		&& previous != new
	{
		findings.push(ambiguous(format!(
			"culture `{old}` has competing rename targets `{previous}` and `{new}`"
		)));
	}
}

fn same_value(left: &AstValue, right: &AstValue) -> bool {
	match (left, right) {
		(AstValue::Scalar { value: left, .. }, AstValue::Scalar { value: right, .. }) => {
			left == right
		}
		(AstValue::Block { items: left, .. }, AstValue::Block { items: right, .. }) => {
			let semantic =
				|statement: &&AstStatement| !matches!(statement, AstStatement::Comment { .. });
			let mut left = left.iter().filter(semantic);
			let mut right = right.iter().filter(semantic);
			loop {
				match (left.next(), right.next()) {
					(None, None) => return true,
					(
						Some(AstStatement::Assignment {
							key: left_key,
							value: left,
							..
						}),
						Some(AstStatement::Assignment {
							key: right_key,
							value: right,
							..
						}),
					) if left_key == right_key && same_value(left, right) => {}
					(
						Some(AstStatement::Item { value: left, .. }),
						Some(AstStatement::Item { value: right, .. }),
					) if same_value(left, right) => {}
					_ => return false,
				}
			}
		}
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::script::parser::parse_clausewitz_content;

	fn game_path(text: &str) -> crate::model::GamePathBuf {
		crate::model::GamePathBuf::parse(text).expect("test game path")
	}

	fn index(source: &str) -> CultureIndex {
		let path = game_path("common/cultures/test.txt");
		let parsed = parse_clausewitz_content(&path, source);
		assert!(parsed.diagnostics.is_empty());
		CultureIndex::from_documents(&[(path, parsed.ast)]).unwrap()
	}

	#[test]
	fn reviewed_correspondence_accepts_a_changed_body_without_rewriting_labels() {
		let base = index("g = { old_culture = { male_names = { Johann } } }");
		let revision = index("g = { new_culture = { male_names = { Otto } } }");
		let reviewed = BTreeMap::from([("old_culture".into(), "new_culture".into())]);
		let result = CultureCorrespondence::infer_observations(
			&base,
			&[CultureRevision {
				parent: &base,
				revision: &revision,
				reviewed: &reviewed,
			}],
		)
		.expect("reviewed identity");
		assert_eq!(result.renames(), &reviewed);
		assert_eq!(
			result.identity(&["g".into()], "new_culture"),
			Some("old_culture")
		);
		assert!(base.definitions.contains_key("old_culture"));
		assert!(revision.definitions.contains_key("new_culture"));
	}

	#[test]
	fn reviewed_correspondence_requires_parent_relative_absent_endpoints() {
		let base = index("g = { old_culture = { male_names = { Johann } } }");
		let reviewed = BTreeMap::from([("old_culture".into(), "new_culture".into())]);
		for source in [
			"g = { old_culture = {} new_culture = {} }",
			"g = { unrelated_culture = {} }",
		] {
			let revision = index(source);
			assert!(
				CultureCorrespondence::infer_observations(
					&base,
					&[CultureRevision {
						parent: &base,
						revision: &revision,
						reviewed: &reviewed,
					}]
				)
				.is_err(),
				"accepted {source}"
			);
		}
		let parent = index("g = { new_culture = {} }");
		assert!(
			CultureCorrespondence::infer_observations(
				&base,
				&[CultureRevision {
					parent: &parent,
					revision: &parent,
					reviewed: &reviewed,
				}]
			)
			.is_err()
		);
	}

	#[test]
	fn correspondence_retains_a_sibling_move_of_the_original_renamed_identity() {
		let base =
			index("g = { country = { discipline = 0.1 } old_culture = { primary = AAA } } h = {}");
		let renamed =
			index("g = { country = { discipline = 0.1 } new_culture = { primary = AAA } } h = {}");
		let moved =
			index("g = { country = { discipline = 0.1 } } h = { old_culture = { primary = AAA } }");
		let correspondence = CultureCorrespondence::infer(&base, &[renamed, moved]).unwrap();
		assert_eq!(
			correspondence
				.renames()
				.get("old_culture")
				.map(String::as_str),
			Some("new_culture")
		);
		assert_eq!(
			correspondence.group_moves.len(),
			1,
			"renaming in one branch must not hide another branch's group move"
		);
		let movement = &correspondence.group_moves[0];
		assert_eq!(
			(movement.old.as_str(), movement.new.as_str()),
			("old_culture", "new_culture")
		);
		assert_eq!((movement.from.as_str(), movement.to.as_str()), ("g", "h"));
		assert_eq!(movement.inherited_fields[0].0, "country");
	}

	#[test]
	fn correspondence_rejects_competing_targets_and_fusion() {
		let base = index("g = { old_culture = {} }");
		assert!(
			CultureCorrespondence::infer(
				&base,
				&[
					index("g = { first_culture = {} }"),
					index("g = { second_culture = {} }"),
				]
			)
			.is_err()
		);
		assert!(
			CultureCorrespondence::infer(
				&index("g = { first_culture = {} second_culture = {} }"),
				&[index("g = { merged_culture = {} }"),]
			)
			.is_err()
		);
	}

	#[test]
	fn correspondence_preserves_split_and_rejects_coexisting_aliases() {
		let base = index("g = { old_culture = {} }");
		let split = index("g = { old_culture = {} new_culture = {} }");
		let correspondence =
			CultureCorrespondence::infer(&base, std::slice::from_ref(&split)).unwrap();
		assert!(correspondence.renames().is_empty());
		assert!(
			CultureCorrespondence::infer(&base, &[split, index("g = { new_culture = {} }")])
				.is_err()
		);
	}

	#[test]
	fn correspondence_ignores_trivia_without_rewriting_labels() {
		let base = index("g = { old_culture = { male_names = { Johann } } }");
		let revision =
			index("h = { new_culture = { # source comment\n male_names = { Johann } } }");
		let correspondence = CultureCorrespondence::infer(&base, &[revision]).unwrap();
		assert_eq!(correspondence.renames()["old_culture"], "new_culture");
		assert_eq!(
			correspondence.identity(&["h".into()], "new_culture"),
			Some("old_culture")
		);
		assert_eq!(
			correspondence.identity(&["h".into(), "new_culture".into()], "male_names"),
			None
		);
	}
}
