//! A translation mod replaces the mod it translates in display text.
//!
//! Two signals name that relationship. An explicit
//! `# foch: translation-of = <mod id or name>` comment is authoritative.
//! Otherwise a mod is a translation only when it carries a `Translation` tag
//! and its `dependencies` name another mod in the same merge. Display names
//! and a tag alone never count. The replacement applies to `tooltip`, `desc`
//! and `custom_tooltip`; every other field merges as it would without one.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::model::{GamePathBuf, MergeReportTranslationAdoption};
use crate::playset::dependency::ModIdentityIndex;

use super::kernel::RevisionId;

/// Which signal named the relationship.
pub(crate) type TranslationBasis = crate::model::TranslationAdoptionBasis;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranslationRelation {
	pub translator: String,
	pub source: String,
	pub basis: TranslationBasis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Adoption {
	path: String,
	field: String,
	translator_mod_id: String,
	source_mod_id: String,
	basis: TranslationBasis,
	adopted: String,
}

/// Relations for one merge, plus the display-text choices they produced.
#[derive(Debug, Default)]
pub(crate) struct TranslationPlan {
	relations: Vec<TranslationRelation>,
	decisions: Mutex<Vec<Adoption>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Frame {
	plan: Option<Arc<TranslationPlan>>,
	/// `RevisionId` 1 is `revisions[0]`, in the order the kernel merges.
	revisions: Vec<String>,
	path: String,
}

thread_local! {
	static FRAME: RefCell<Frame> = RefCell::new(Frame::default());
}

#[must_use]
pub(crate) struct FrameGuard {
	previous: Frame,
}

impl Drop for FrameGuard {
	fn drop(&mut self) {
		let previous = std::mem::take(&mut self.previous);
		FRAME.with(|frame| *frame.borrow_mut() = previous);
	}
}

impl TranslationPlan {
	pub(crate) fn from_relations(relations: Vec<TranslationRelation>) -> Arc<Self> {
		Arc::new(Self {
			relations,
			decisions: Mutex::new(Vec::new()),
		})
	}

	/// Relations among `mods`. An explicit comment in `descriptor.mod` wins
	/// over the tag-and-dependency rule for that mod.
	pub(crate) fn from_mods(mods: &[crate::model::ModCandidate]) -> Arc<Self> {
		let index = ModIdentityIndex::from_mods(mods);
		let mut relations = Vec::new();
		for candidate in mods {
			let Some(descriptor) = candidate.descriptor.as_ref() else {
				continue;
			};
			let explicit = explicit_targets_of(candidate);
			let resolved = explicit
				.iter()
				.filter_map(|target| other_mod(&index, mods, &candidate.mod_id, target))
				.collect::<Vec<_>>();
			if !resolved.is_empty() {
				for source in resolved {
					push_relation(
						&mut relations,
						candidate.mod_id.clone(),
						source,
						TranslationBasis::Explicit,
					);
				}
				continue;
			}
			if !has_translation_tag(&descriptor.tags) {
				continue;
			}
			for dependency in &descriptor.dependencies {
				if let Some(source) = other_mod(&index, mods, &candidate.mod_id, dependency) {
					push_relation(
						&mut relations,
						candidate.mod_id.clone(),
						source,
						TranslationBasis::TagAndDependency,
					);
				}
			}
		}
		Self::from_relations(relations)
	}

	/// Install this plan for the current thread until the guard drops.
	pub(crate) fn enter(self: &Arc<Self>) -> FrameGuard {
		let mut frame = current_frame();
		frame.plan = Some(Arc::clone(self));
		swap_frame(frame)
	}

	pub(crate) fn report_adoptions(&self) -> Vec<MergeReportTranslationAdoption> {
		let mut decisions = self
			.decisions
			.lock()
			.unwrap_or_else(|error| error.into_inner())
			.clone();
		decisions.sort_by(|left, right| {
			(
				&left.path,
				&left.field,
				&left.translator_mod_id,
				&left.source_mod_id,
			)
				.cmp(&(
					&right.path,
					&right.field,
					&right.translator_mod_id,
					&right.source_mod_id,
				))
		});
		decisions.dedup();
		decisions
			.into_iter()
			.filter_map(|adoption| {
				Some(MergeReportTranslationAdoption {
					path: GamePathBuf::parse(&adoption.path).ok()?,
					field: adoption.field,
					translator_mod_id: adoption.translator_mod_id,
					source_mod_id: adoption.source_mod_id,
					basis: adoption.basis,
					adopted: adoption.adopted,
				})
			})
			.collect()
	}
}

/// The mod id or name a `# foch: translation-of = …` comment names.
pub fn translation_of_target(comment: &str) -> Option<String> {
	let rest = translation_of_body(comment)?;
	let value = scalar_value(rest).ok()?;
	(!value.is_empty()).then_some(value)
}

/// `Some` when the comment is a translation declaration that names nothing.
pub fn translation_of_error(comment: &str) -> Option<&'static str> {
	let rest = translation_of_body(comment)?;
	match scalar_value(rest) {
		Ok(value) if !value.is_empty() => None,
		_ => Some("expected `# foch: translation-of = <mod id or name>`"),
	}
}

/// Every translation target declared in `text`, in file order.
pub(crate) fn explicit_targets_in(text: &str) -> Vec<String> {
	let text = text.trim_start_matches('\u{feff}');
	let mut targets = Vec::new();
	for line in text.lines() {
		let Some(comment) = line.trim().strip_prefix('#') else {
			continue;
		};
		if let Some(target) = translation_of_target(comment)
			&& !targets.contains(&target)
		{
			targets.push(target);
		}
	}
	targets
}

pub(crate) fn set_revisions(revisions: Vec<String>, path: String) -> FrameGuard {
	let mut frame = current_frame();
	frame.revisions = revisions;
	frame.path = path;
	swap_frame(frame)
}

pub(crate) fn snapshot() -> Frame {
	current_frame()
}

impl Frame {
	pub(crate) fn install(&self) -> FrameGuard {
		swap_frame(self.clone())
	}
}

/// One revision's display text kept, or several texts still shown together.
pub(crate) enum TextChoice {
	Keep(RevisionId),
	Combine(String),
}

/// Replace a translated mod's display text with its translation.
///
/// `ordered` is the changed values in load order. `None` leaves the caller
/// on the ordinary concatenation path.
pub(crate) fn choose_text(
	field_path: &[String],
	ordered: &[(RevisionId, &str)],
) -> Option<TextChoice> {
	let field = field_path.last().map(String::as_str)?;
	if !super::combined_text::is_text_field(field) || ordered.len() < 2 {
		return None;
	}
	let frame = current_frame();
	let plan = frame.plan.as_ref()?;
	if plan.relations.is_empty() || frame.revisions.is_empty() {
		return None;
	}
	let mut entries = Vec::with_capacity(ordered.len());
	for (revision, text) in ordered {
		let index = revision.get().checked_sub(1)? as usize;
		let mod_id = frame.revisions.get(index)?.clone();
		entries.push(Entry {
			revision: *revision,
			mod_id,
			text: (*text).to_string(),
		});
	}
	let mut dropped = false;
	let mut kept = Vec::new();
	// Recorded only when this call returns a choice. A full mutual replacement
	// declines and the caller concatenates every text, so those rows must not
	// stay on the plan.
	let mut pending = Vec::new();
	for entry in &entries {
		let translators = plan
			.relations
			.iter()
			.filter(|relation| {
				relation.source == entry.mod_id
					&& entries
						.iter()
						.any(|other| other.mod_id == relation.translator)
			})
			.collect::<Vec<_>>();
		let replaced = translators.iter().any(|relation| {
			entries
				.iter()
				.any(|other| other.mod_id == relation.translator && other.text != entry.text)
		});
		if !replaced {
			kept.push(entry.clone());
			continue;
		}
		dropped = true;
		for relation in translators {
			let Some(translator) = entries
				.iter()
				.find(|other| other.mod_id == relation.translator && other.text != entry.text)
			else {
				continue;
			};
			pending.push(Adoption {
				path: frame.path.clone(),
				field: field_path.join("/"),
				translator_mod_id: translator.mod_id.clone(),
				source_mod_id: entry.mod_id.clone(),
				basis: relation.basis,
				adopted: translator.text.clone(),
			});
		}
	}
	if !dropped || kept.is_empty() {
		return None;
	}
	let mut distinct = Vec::new();
	for entry in &kept {
		if !distinct.iter().any(|text: &String| text == &entry.text) {
			distinct.push(entry.text.clone());
		}
	}
	let choice = if distinct.len() == 1 {
		let chosen = kept
			.iter()
			.filter(|entry| {
				plan.relations
					.iter()
					.any(|relation| relation.translator == entry.mod_id)
			})
			.max_by_key(|entry| entry.revision)
			.unwrap_or(kept.last().expect("kept is non-empty"));
		TextChoice::Keep(chosen.revision)
	} else {
		let combined = super::combined_text::combine(distinct.iter().map(String::as_str))?;
		TextChoice::Combine(combined)
	};
	for adoption in pending {
		plan.record(adoption);
	}
	Some(choice)
}

#[derive(Clone)]
struct Entry {
	revision: RevisionId,
	mod_id: String,
	text: String,
}

impl TranslationPlan {
	fn record(&self, adoption: Adoption) {
		self.decisions
			.lock()
			.unwrap_or_else(|error| error.into_inner())
			.push(adoption);
	}
}

fn current_frame() -> Frame {
	FRAME.with(|frame| frame.borrow().clone())
}

fn swap_frame(frame: Frame) -> FrameGuard {
	FRAME.with(|slot| FrameGuard {
		previous: std::mem::replace(&mut *slot.borrow_mut(), frame),
	})
}

fn translation_of_body(comment: &str) -> Option<&str> {
	let rest = comment.trim().strip_prefix("foch:")?.trim_start();
	let rest = rest.strip_prefix("translation-of")?;
	if !rest.is_empty()
		&& !rest.starts_with(|character: char| character.is_whitespace() || character == '=')
	{
		return None;
	}
	rest.trim_start().strip_prefix('=')
}

fn scalar_value(rest: &str) -> Result<String, ()> {
	let rest = rest.trim();
	if rest.is_empty() {
		return Err(());
	}
	if let Some(quoted) = rest.strip_prefix('"') {
		let mut value = String::new();
		let mut characters = quoted.chars();
		while let Some(character) = characters.next() {
			if character == '\\' {
				value.push(characters.next().ok_or(())?);
			} else if character == '"' {
				return Ok(value);
			} else {
				value.push(character);
			}
		}
		return Err(());
	}
	Ok(rest.to_string())
}

fn explicit_targets_of(candidate: &crate::model::ModCandidate) -> Vec<String> {
	let Some(path) = descriptor_file(candidate) else {
		return Vec::new();
	};
	let Ok(bytes) = fs::read(path) else {
		return Vec::new();
	};
	explicit_targets_in(&String::from_utf8_lossy(&bytes))
}

fn descriptor_file(candidate: &crate::model::ModCandidate) -> Option<PathBuf> {
	if let Some(path) = &candidate.descriptor_path {
		return Some(path.clone());
	}
	let path = candidate.root_path.as_ref()?.join("descriptor.mod");
	path.is_file().then_some(path)
}

fn has_translation_tag(tags: &[String]) -> bool {
	tags.iter()
		.any(|tag| tag.eq_ignore_ascii_case("Translation"))
}

fn other_mod(
	index: &ModIdentityIndex,
	mods: &[crate::model::ModCandidate],
	translator: &str,
	token: &str,
) -> Option<String> {
	let source = &mods[index.lookup(token)?];
	(source.mod_id != translator && !source.mod_id.is_empty()).then(|| source.mod_id.clone())
}

fn push_relation(
	relations: &mut Vec<TranslationRelation>,
	translator: String,
	source: String,
	basis: TranslationBasis,
) {
	if relations
		.iter()
		.any(|relation| relation.translator == translator && relation.source == source)
	{
		return;
	}
	relations.push(TranslationRelation {
		translator,
		source,
		basis,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::script::emit::emit_clausewitz_statements;
	use crate::game::eu4::script::parser::parse_clausewitz_content;
	use crate::model::ModCandidate;
	use crate::playset::PlaysetEntry;
	use crate::playset::descriptor::ModDescriptor;

	fn candidate(mod_id: &str, name: &str, tags: &[&str], dependencies: &[&str]) -> ModCandidate {
		ModCandidate {
			entry: PlaysetEntry {
				steam_id: Some(mod_id.to_string()),
				display_name: Some(name.to_string()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
				dependencies: dependencies.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn parse_button(tooltip_line: &str, extra: &str) -> crate::game::eu4::script::parser::AstFile {
		let source = format!("custom_button = {{\n\tname = b\n\t{tooltip_line}\n\t{extra}}}\n");
		let parsed = parse_clausewitz_content(
			&GamePathBuf::parse("common/custom_gui/buttons.txt").expect("path"),
			&source,
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn emit(file: &crate::game::eu4::script::parser::AstFile) -> String {
		emit_clausewitz_statements(&file.statements).expect("emit")
	}

	fn relation(translator: &str, source: &str, basis: TranslationBasis) -> TranslationRelation {
		TranslationRelation {
			translator: translator.to_string(),
			source: source.to_string(),
			basis,
		}
	}

	#[test]
	fn comment_names_an_id_or_a_quoted_name() {
		assert_eq!(
			translation_of_target("foch: translation-of = 2164202838"),
			Some("2164202838".to_string())
		);
		assert_eq!(
			translation_of_target("foch: translation-of = \"Europa Expanded\""),
			Some("Europa Expanded".to_string())
		);
		assert_eq!(
			translation_of_target("foch: translation-of = Europa Expanded"),
			Some("Europa Expanded".to_string())
		);
		assert_eq!(
			explicit_targets_in("# foch: translation-of = 2164202838\nname=\"欧陆拓展\"\n"),
			vec!["2164202838".to_string()]
		);
		assert!(translation_of_target("foch: button from Europa Expanded").is_none());
		assert!(translation_of_error("foch: translation-of =").is_some());
		assert!(translation_of_error("foch: translation-of = \"Europa").is_some());
	}

	#[test]
	fn tag_and_dependency_names_only_a_participating_mod() {
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &["Gameplay"], &[]),
			candidate(
				"2411504869",
				"欧陆拓展-未知领域",
				&["Translation"],
				&["Europa Expanded"],
			),
		];
		let plan = TranslationPlan::from_mods(&mods);
		assert_eq!(
			plan.relations,
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::TagAndDependency
			)]
		);
	}

	#[test]
	fn translation_tag_without_a_participating_dependency_is_not_a_relation() {
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			candidate(
				"2411504869",
				"欧陆拓展-未知领域",
				&["Translation"],
				&["Some Other Mod"],
			),
		];
		assert!(TranslationPlan::from_mods(&mods).relations.is_empty());
	}

	#[test]
	fn dependency_without_the_translation_tag_is_not_a_relation() {
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			candidate(
				"2411504869",
				"欧陆拓展-未知领域",
				&["Gameplay"],
				&["Europa Expanded"],
			),
		];
		assert!(TranslationPlan::from_mods(&mods).relations.is_empty());
	}

	#[test]
	fn a_translation_like_name_alone_is_not_a_relation() {
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			candidate("2411504869", "Europa Expanded 中文翻译", &[], &[]),
		];
		assert!(TranslationPlan::from_mods(&mods).relations.is_empty());
	}

	#[test]
	fn explicit_marker_wins_over_a_different_dependency() {
		let mut translator = candidate(
			"2411504869",
			"欧陆拓展-未知领域",
			&["Translation"],
			&["Other Gameplay"],
		);
		let directory = tempfile::tempdir().expect("descriptor dir");
		let descriptor = directory.path().join("descriptor.mod");
		fs::write(
			&descriptor,
			"name=\"欧陆拓展-未知领域\"\ntags={\n\tTranslation\n}\ndependencies={\n\t\"Other Gameplay\"\n}\n# foch: translation-of = 2164202838\n",
		)
		.expect("write descriptor");
		translator.descriptor_path = Some(descriptor);
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			candidate("999", "Other Gameplay", &[], &[]),
			translator,
		];
		assert_eq!(
			TranslationPlan::from_mods(&mods).relations,
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::Explicit
			)]
		);
	}

	#[test]
	fn unresolved_explicit_marker_falls_back_to_tag_and_dependency() {
		let mut translator = candidate(
			"2411504869",
			"欧陆拓展-未知领域",
			&["translation"],
			&["Europa Expanded"],
		);
		let directory = tempfile::tempdir().expect("descriptor dir");
		let descriptor = directory.path().join("descriptor.mod");
		fs::write(&descriptor, "# foch: translation-of = 000\n").expect("write descriptor");
		translator.descriptor_path = Some(descriptor);
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			translator,
		];
		assert_eq!(
			TranslationPlan::from_mods(&mods).relations,
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::TagAndDependency
			)]
		);
	}

	fn merge_tooltips(
		field: &str,
		left: &str,
		right: &str,
		extra_left: &str,
		extra_right: &str,
		relations: Vec<TranslationRelation>,
	) -> (
		crate::merge::structured::ClausewitzMergeOutcome,
		Vec<MergeReportTranslationAdoption>,
	) {
		let base = parse_button(&format!("{field} = BASE_TT"), "cost = 1\n");
		let gameplay = parse_button(&format!("{field} = {left}"), extra_left);
		let translation = parse_button(&format!("{field} = {right}"), extra_right);
		let plan = TranslationPlan::from_relations(relations);
		let _scope = swap_frame(Frame {
			plan: Some(Arc::clone(&plan)),
			revisions: vec!["2164202838".to_string(), "2411504869".to_string()],
			path: "common/custom_gui/buttons.txt".to_string(),
		});
		let outcome = crate::merge::structured::merge_clausewitz_files_n_way(
			&base,
			&[&gameplay, &translation],
			&crate::game::eu4::content::MergePolicies::default(),
		)
		.expect("merge");
		(outcome, plan.report_adoptions())
	}

	#[test]
	fn explicit_translation_replaces_display_text_instead_of_concatenating() {
		for field in ["tooltip", "desc", "custom_tooltip"] {
			let (outcome, adoptions) = merge_tooltips(
				field,
				"LEFT_TT",
				"\"译文\"",
				"cost = 1\n",
				"cost = 1\n",
				vec![relation(
					"2411504869",
					"2164202838",
					TranslationBasis::Explicit,
				)],
			);
			assert!(
				outcome.conflicts().is_empty(),
				"{field} {:?}",
				outcome.conflicts()
			);
			let output = emit(outcome.resolved_ast().expect("resolved"));
			assert!(!output.contains("FOCH_TEXT_"), "{field} {output}");
			assert!(output.contains("译文"), "{field} {output}");
			assert!(!output.contains("LEFT_TT"), "{field} {output}");
			assert_eq!(adoptions.len(), 1, "{field} {adoptions:?}");
			assert_eq!(adoptions[0].basis, TranslationBasis::Explicit);
			assert_eq!(adoptions[0].translator_mod_id, "2411504869");
			assert_eq!(adoptions[0].source_mod_id, "2164202838");
			assert_eq!(adoptions[0].adopted, "译文");
			assert!(
				adoptions[0].field.ends_with(field),
				"{}",
				adoptions[0].field
			);
		}
	}

	#[test]
	fn tag_and_dependency_uses_the_translation_for_europa_expanded() {
		let (outcome, adoptions) = merge_tooltips(
			"tooltip",
			"EE_TT",
			"EE_ZH_TT",
			"cost = 1\n",
			"cost = 1\n",
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::TagAndDependency,
			)],
		);
		let output = emit(outcome.resolved_ast().expect("resolved"));
		assert!(!output.contains("FOCH_TEXT_"), "{output}");
		assert!(output.contains("EE_ZH_TT"), "{output}");
		assert!(!output.contains("EE_TT"), "{output}");
		assert_eq!(adoptions[0].basis, TranslationBasis::TagAndDependency);
	}

	#[test]
	fn mods_without_a_translation_relation_still_concatenate() {
		let (outcome, adoptions) = merge_tooltips(
			"tooltip",
			"LEFT_TT",
			"RIGHT_TT",
			"cost = 1\n",
			"cost = 1\n",
			Vec::new(),
		);
		let output = emit(outcome.resolved_ast().expect("resolved"));
		let combined = crate::merge::combined_text::keys_in(&output);
		assert_eq!(combined.len(), 1, "{output}");
		assert!(adoptions.is_empty());
	}

	#[test]
	fn a_translation_tag_alone_still_concatenates() {
		let (outcome, adoptions) = merge_tooltips(
			"tooltip",
			"LEFT_TT",
			"RIGHT_TT",
			"cost = 1\n",
			"cost = 1\n",
			Vec::new(),
		);
		assert!(outcome.resolved_ast().is_some());
		assert!(adoptions.is_empty());
		let mods = vec![
			candidate("2164202838", "Europa Expanded", &[], &[]),
			candidate("2411504869", "欧陆拓展-未知领域", &["Translation"], &[]),
		];
		assert!(TranslationPlan::from_mods(&mods).relations.is_empty());
	}

	#[test]
	fn a_non_text_field_still_conflicts_when_display_text_is_translated() {
		let (outcome, adoptions) = merge_tooltips(
			"tooltip",
			"LEFT_TT",
			"\"译文\"",
			"cost = 2\n",
			"cost = 3\n",
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::TagAndDependency,
			)],
		);
		assert!(!outcome.conflicts().is_empty(), "cost must stay a conflict");
		let output = emit(outcome.tentative_ast());
		assert!(!output.contains("FOCH_TEXT_"), "{output}");
		assert!(output.contains("译文"), "{output}");
		assert!(!adoptions.is_empty());
	}

	/// The production joins install revision ids themselves. This does not
	/// build a `Frame` by hand: `enter` supplies the plan, and the kernel
	/// supplies the mod ids and path before `choose_text` runs.
	fn merge_through_production_join(
		join: &dyn crate::merge::structured::TreeJoinProtocol,
		field: &str,
		left: &str,
		right: &str,
		relations: Vec<TranslationRelation>,
	) -> (String, Vec<MergeReportTranslationAdoption>) {
		let base = parse_button(&format!("{field} = BASE_TT"), "cost = 1\n");
		let gameplay = parse_button(&format!("{field} = {left}"), "cost = 1\n");
		let translation = parse_button(&format!("{field} = {right}"), "cost = 1\n");
		let plan = TranslationPlan::from_relations(relations);
		let _entered = plan.enter();
		let policies = crate::game::eu4::content::MergePolicies::default();
		let kernel = crate::merge::structured::TreeMergeKernel::new(join, &policies);
		let input = crate::merge::kernel_adapter::KernelMergeInput::new(
			base,
			vec![
				crate::merge::kernel_adapter::KernelRevision {
					source_id: "2164202838".to_string(),
					precedence: 10,
					ast: gameplay,
				},
				crate::merge::kernel_adapter::KernelRevision {
					source_id: "2411504869".to_string(),
					precedence: 20,
					ast: translation,
				},
			],
		);
		let outcome = kernel.merge_tentative(&input).expect("production merge");
		assert!(
			outcome.conflicts.is_empty(),
			"{field} {:?}",
			outcome.conflicts
		);
		let output = emit_clausewitz_statements(&outcome.statements).expect("emit");
		(output, plan.report_adoptions())
	}

	#[test]
	fn file_join_replaces_display_text_without_a_hand_built_frame() {
		let (output, adoptions) = merge_through_production_join(
			&crate::merge::structured::ClausewitzFileJoin,
			"tooltip",
			"LEFT_TT",
			"\"译文\"",
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::Explicit,
			)],
		);
		assert!(!output.contains("FOCH_TEXT_"), "{output}");
		assert!(output.contains("译文"), "{output}");
		assert!(!output.contains("LEFT_TT"), "{output}");
		assert_eq!(adoptions.len(), 1, "{adoptions:?}");
		assert_eq!(adoptions[0].translator_mod_id, "2411504869");
		assert_eq!(adoptions[0].source_mod_id, "2164202838");
		assert_eq!(adoptions[0].adopted, "译文");
		assert!(
			adoptions[0].field.ends_with("tooltip"),
			"{}",
			adoptions[0].field
		);
		assert_eq!(adoptions[0].path.as_str(), "common/custom_gui/buttons.txt");
	}

	#[test]
	fn definition_module_join_replaces_display_text_without_a_hand_built_frame() {
		let (output, adoptions) = merge_through_production_join(
			&crate::merge::structured::DefinitionModuleJoin,
			"desc",
			"LEFT_TT",
			"\"译文\"",
			vec![relation(
				"2411504869",
				"2164202838",
				TranslationBasis::Explicit,
			)],
		);
		assert!(!output.contains("FOCH_TEXT_"), "{output}");
		assert!(output.contains("译文"), "{output}");
		assert!(!output.contains("LEFT_TT"), "{output}");
		assert_eq!(adoptions.len(), 1, "{adoptions:?}");
		assert_eq!(adoptions[0].basis, TranslationBasis::Explicit);
		assert_eq!(adoptions[0].adopted, "译文");
	}

	#[test]
	fn mutual_translation_markers_concatenate_and_record_no_adoption() {
		let (output, adoptions) = merge_through_production_join(
			&crate::merge::structured::ClausewitzFileJoin,
			"tooltip",
			"LEFT_TT",
			"RIGHT_TT",
			vec![
				relation("2411504869", "2164202838", TranslationBasis::Explicit),
				relation("2164202838", "2411504869", TranslationBasis::Explicit),
			],
		);
		let combined = crate::merge::combined_text::keys_in(&output);
		assert_eq!(combined.len(), 1, "{output}");
		let versions = crate::merge::combined_text::versions(&combined[0]).expect("versions");
		assert!(
			versions.iter().any(|version| version.contains("LEFT_TT")),
			"{versions:?}"
		);
		assert!(
			versions.iter().any(|version| version.contains("RIGHT_TT")),
			"{versions:?}"
		);
		assert!(
			adoptions.is_empty(),
			"declined replacement must not report an adoption: {adoptions:?}"
		);
	}
}
