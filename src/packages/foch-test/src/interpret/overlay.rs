//! Ordered playset overlay for the offline model.
//!
//! Layers arrive in playset order: vanilla, then each mod in the order the
//! playset lists them. This module does not sort layers. A later layer
//! replaces the same relative path outright and, for decisions, replaces the
//! same decision name. `replace_path` drops every earlier file under that
//! directory before the layer's own files are added. Country and province
//! identity comes from the files that survive, never from a built-in tag or
//! province list.

use super::{ProvinceSeed, SeedFacts, SeededCountry};
use crate::collect::country_decisions;
use crate::model::{DecisionInfo, GameDate, Tag};
use crate::source::RelPath;
use foch::game::eu4::script::parser::{AstStatement, AstValue, parse_clausewitz_content};
use foch_annotation::value::scalar;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One layer of an ordered playset. Vanilla has an empty `replace_paths`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentLayer {
	/// `replace_path` directories, game-relative, `/`-separated.
	pub replace_paths: Vec<String>,
	/// Files this layer ships. Within one layer the caller chooses the
	/// order; the overlay applies them in that order and does not sort
	/// layers against each other.
	pub files: Vec<LayerFile>,
}

/// One game-relative file and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerFile {
	pub path: String,
	pub text: String,
}

/// History that still depends on the case date. Fold it with [`fold_history`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryTimeline {
	pub countries: BTreeMap<Tag, Vec<CountrySlice>>,
	pub provinces: BTreeMap<u32, Vec<ProvinceSlice>>,
}

/// One history assignment group: the file preamble (`date == None`) or one
/// date block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CountrySlice {
	pub date: Option<GameDate>,
	pub religion: Option<String>,
	pub culture: Option<String>,
	pub government: Option<String>,
	pub government_rank: Option<i64>,
}

/// One province history assignment group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProvinceSlice {
	pub date: Option<GameDate>,
	pub owner: Option<Tag>,
	pub controller: Option<Tag>,
	pub religion: Option<String>,
	pub culture: Option<String>,
}

/// Decisions visible after vanilla and every mod, in playset order.
///
/// The same decision name keeps the later definition. `replace_path` of
/// `decisions` (or a directory above it) drops every earlier decision first.
pub fn overlay_decisions(layers: &[ContentLayer]) -> Result<Vec<DecisionInfo>, String> {
	let mut by_name: BTreeMap<String, DecisionInfo> = BTreeMap::new();
	let mut order: Vec<String> = Vec::new();
	// Surviving files only: a later layer's copy of the same path drops the
	// earlier file, so names that existed only there disappear. Remaining
	// files apply in playset order, and a repeated name keeps the later body.
	for file in visible_files(layers)
		.into_iter()
		.filter(|file| is_under(&file.path, "decisions"))
	{
		let path = RelPath::new(&file.path)?;
		let statements = parse_file(&file.path, &file.text)?;
		for decision in country_decisions(&path, &file.text, &statements) {
			if !by_name.contains_key(&decision.name) {
				order.push(decision.name.clone());
			}
			by_name.insert(decision.name.clone(), decision);
		}
	}
	Ok(order
		.into_iter()
		.filter_map(|name| by_name.remove(&name))
		.collect())
}

/// Country and province history after the same ordered overlay.
///
/// A later file at the same relative path replaces the earlier file
/// entirely, including its date blocks. Two different paths that name the
/// same tag or province id: the later layer's file wins.
pub fn overlay_history(layers: &[ContentLayer]) -> Result<HistoryTimeline, String> {
	let mut countries: BTreeMap<Tag, Vec<CountrySlice>> = BTreeMap::new();
	let mut provinces: BTreeMap<u32, Vec<ProvinceSlice>> = BTreeMap::new();
	let files = visible_files(layers);
	for file in files
		.iter()
		.filter(|file| is_under(&file.path, "history/countries"))
	{
		let Some(tag) = tag_from_country_path(&file.path) else {
			continue;
		};
		countries.insert(tag, country_slices(&file.path, &file.text)?);
	}
	for file in files
		.iter()
		.filter(|file| is_under(&file.path, "history/provinces"))
	{
		let Some(id) = province_id(&file.path) else {
			continue;
		};
		provinces.insert(id, province_slices(&file.path, &file.text)?);
	}
	Ok(HistoryTimeline {
		countries,
		provinces,
	})
}

/// The timeline's state on `date`. Preamble always applies. A date block
/// applies when its date is on or before `date`, in chronological order.
/// Tags and province ids are exactly the keys that survived the overlay.
pub fn fold_history(timeline: &HistoryTimeline, date: GameDate) -> SeedFacts {
	let mut countries = BTreeMap::new();
	for (tag, slices) in &timeline.countries {
		countries.insert(tag.clone(), fold_country(slices, date));
	}
	let mut provinces = BTreeMap::new();
	for (id, slices) in &timeline.provinces {
		provinces.insert(*id, fold_province(slices, date));
	}
	SeedFacts {
		tags: countries.keys().cloned().collect(),
		countries,
		provinces,
	}
}

fn fold_country(slices: &[CountrySlice], date: GameDate) -> SeededCountry {
	let mut state = SeededCountry::default();
	for slice in slices.iter().filter(|slice| slice.date.is_none()) {
		if let Some(value) = &slice.religion {
			state.religion = Some(value.clone());
		}
		if let Some(value) = &slice.culture {
			state.culture = Some(value.clone());
		}
		if let Some(value) = &slice.government {
			state.government = Some(value.clone());
		}
		if let Some(value) = slice.government_rank {
			state.government_rank = Some(value);
		}
	}
	let mut dated: Vec<&CountrySlice> =
		slices.iter().filter(|slice| slice.date.is_some()).collect();
	dated.sort_by_key(|slice| slice.date);
	for slice in dated {
		if slice.date.is_some_and(|when| when <= date) {
			if let Some(value) = &slice.religion {
				state.religion = Some(value.clone());
			}
			if let Some(value) = &slice.culture {
				state.culture = Some(value.clone());
			}
			if let Some(value) = &slice.government {
				state.government = Some(value.clone());
			}
			if let Some(value) = slice.government_rank {
				state.government_rank = Some(value);
			}
		}
	}
	state
}

fn fold_province(slices: &[ProvinceSlice], date: GameDate) -> ProvinceSeed {
	let mut state = ProvinceSeed::default();
	let apply = |state: &mut ProvinceSeed, slice: &ProvinceSlice| {
		if let Some(value) = &slice.owner {
			state.owner = Some(value.clone());
		}
		if let Some(value) = &slice.controller {
			state.controller = Some(value.clone());
		}
		if let Some(value) = &slice.religion {
			state.religion = Some(value.clone());
		}
		if let Some(value) = &slice.culture {
			state.culture = Some(value.clone());
		}
	};
	for slice in slices.iter().filter(|slice| slice.date.is_none()) {
		apply(&mut state, slice);
	}
	let mut dated: Vec<&ProvinceSlice> =
		slices.iter().filter(|slice| slice.date.is_some()).collect();
	dated.sort_by_key(|slice| slice.date);
	for slice in dated {
		if slice.date.is_some_and(|when| when <= date) {
			apply(&mut state, slice);
		}
	}
	state
}

/// Files that survive through the last layer, in application order.
fn visible_files(layers: &[ContentLayer]) -> Vec<LayerFile> {
	let mut visible: Vec<LayerFile> = Vec::new();
	for layer in layers {
		visible.retain(|file| !reset_by(&file.path, &layer.replace_paths));
		for file in &layer.files {
			let path = portable(&file.path);
			if let Some(index) = visible.iter().position(|existing| existing.path == path) {
				visible.remove(index);
			}
			visible.push(LayerFile {
				path,
				text: file.text.clone(),
			});
		}
	}
	visible
}

fn reset_by(path: &str, replace_paths: &[String]) -> bool {
	let path = portable(path);
	replace_paths.iter().any(|prefix| {
		let prefix = portable(prefix);
		let prefix = prefix.trim_matches('/');
		!prefix.is_empty() && (path == prefix || path.starts_with(&format!("{prefix}/")))
	})
}

fn is_under(path: &str, directory: &str) -> bool {
	portable(path)
		.strip_prefix(directory)
		.is_some_and(|rest| rest.starts_with('/'))
}

fn portable(path: &str) -> String {
	path.replace('\\', "/")
}

fn parse_file(path: &str, text: &str) -> Result<Vec<AstStatement>, String> {
	let portable_path = portable(path);
	let label = crate::game_path(&portable_path);
	let parsed = parse_clausewitz_content(label, text);
	if let Some(diagnostic) = parsed.diagnostics.first() {
		return Err(format!("{path}: {}", diagnostic.message));
	}
	Ok(parsed.ast.statements)
}

fn country_slices(path: &str, text: &str) -> Result<Vec<CountrySlice>, String> {
	let mut preamble = CountrySlice::default();
	let mut dated = Vec::new();
	for statement in parse_file(path, text)? {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if let Ok(date) = GameDate::parse(&key) {
			let AstValue::Block { items, .. } = &value else {
				continue;
			};
			let mut slice = CountrySlice {
				date: Some(date),
				..CountrySlice::default()
			};
			for item in items {
				let AstStatement::Assignment { key, value, .. } = item else {
					continue;
				};
				absorb_country(&mut slice, key, value);
			}
			dated.push(slice);
		} else {
			absorb_country(&mut preamble, &key, &value);
		}
	}
	let mut slices = Vec::new();
	if preamble.religion.is_some()
		|| preamble.culture.is_some()
		|| preamble.government.is_some()
		|| preamble.government_rank.is_some()
	{
		slices.push(preamble);
	}
	slices.extend(dated);
	Ok(slices)
}

fn absorb_country(slice: &mut CountrySlice, key: &str, value: &AstValue) {
	let text = scalar(value).ok();
	match key {
		"religion" => slice.religion = text,
		"primary_culture" => slice.culture = text,
		"government" => slice.government = text,
		"government_rank" => slice.government_rank = text.and_then(|text| text.parse().ok()),
		_ => {}
	}
}

fn province_slices(path: &str, text: &str) -> Result<Vec<ProvinceSlice>, String> {
	let mut preamble = ProvinceSlice::default();
	let mut dated = Vec::new();
	for statement in parse_file(path, text)? {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if let Ok(date) = GameDate::parse(&key) {
			let AstValue::Block { items, .. } = &value else {
				continue;
			};
			let mut slice = ProvinceSlice {
				date: Some(date),
				..ProvinceSlice::default()
			};
			for item in items {
				let AstStatement::Assignment { key, value, .. } = item else {
					continue;
				};
				absorb_province(&mut slice, key, value);
			}
			dated.push(slice);
		} else {
			absorb_province(&mut preamble, &key, &value);
		}
	}
	let mut slices = Vec::new();
	if preamble.owner.is_some()
		|| preamble.controller.is_some()
		|| preamble.religion.is_some()
		|| preamble.culture.is_some()
	{
		slices.push(preamble);
	}
	slices.extend(dated);
	Ok(slices)
}

fn absorb_province(slice: &mut ProvinceSlice, key: &str, value: &AstValue) {
	let text = scalar(value).ok();
	match key {
		"owner" => slice.owner = text.and_then(|text| Tag::parse(&text).ok()),
		"controller" => slice.controller = text.and_then(|text| Tag::parse(&text).ok()),
		"religion" => slice.religion = text,
		"culture" => slice.culture = text,
		_ => {}
	}
}

/// The tag is the filename token, not a lookup in a vanilla country list.
fn tag_from_country_path(path: &str) -> Option<Tag> {
	let file = path.rsplit(['/', '\\']).next()?;
	let stem = file.strip_suffix(".txt")?;
	let head = stem.split(" - ").next()?.trim();
	let head = head.split_whitespace().next().unwrap_or(head);
	Tag::parse(head).ok()
}

/// The province id is the leading number of the filename. There is no
/// vanilla maximum: a mod province past the unmodded map is still a province.
fn province_id(path: &str) -> Option<u32> {
	let file = path.rsplit(['/', '\\']).next()?;
	let stem = file.strip_suffix(".txt")?;
	let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
	let id = digits.parse::<u32>().ok()?;
	(id >= 1).then_some(id)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeSet;

	fn layer(replace_paths: &[&str], files: &[(&str, &str)]) -> ContentLayer {
		ContentLayer {
			replace_paths: replace_paths
				.iter()
				.map(|path| (*path).to_string())
				.collect(),
			files: files
				.iter()
				.map(|(path, text)| LayerFile {
					path: (*path).to_string(),
					text: (*text).to_string(),
				})
				.collect(),
		}
	}

	fn names(decisions: &[DecisionInfo]) -> Vec<&str> {
		decisions
			.iter()
			.map(|decision| decision.name.as_str())
			.collect()
	}

	#[test]
	fn later_layers_override_decision_names_even_when_their_filenames_sort_first() {
		let vanilla = layer(
			&[],
			&[(
				"decisions/zzz_vanilla.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = vanilla } }\n\tgone = { effect = { set_country_flag = gone } }\n}\n",
			)],
		);
		let earlier = layer(
			&[],
			&[(
				"decisions/aaa_early.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = earlier } }\n}\n",
			)],
		);
		let later = layer(
			&[],
			&[(
				"decisions/mmm_late.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = later } }\n\textra = { effect = { set_country_flag = extra } }\n}\n",
			)],
		);
		let decisions = overlay_decisions(&[vanilla, earlier, later]).unwrap();
		assert_eq!(names(&decisions), ["keep", "gone", "extra"]);
		let keep = decisions
			.iter()
			.find(|decision| decision.name == "keep")
			.unwrap();
		assert!(
			keep.body.contains("set_country_flag = later"),
			"the later mod wins, not the filename that sorts last: {}",
			keep.body
		);
	}

	#[test]
	fn replace_path_drops_earlier_decisions_before_the_layer_adds_its_own() {
		let vanilla = layer(
			&[],
			&[(
				"decisions/00.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = vanilla } }\n\tgone = { effect = { set_country_flag = gone } }\n}\n",
			)],
		);
		let resetting = layer(
			&["decisions"],
			&[(
				"decisions/mod.txt",
				"country_decisions = {\n\tfresh = { effect = { set_country_flag = fresh } }\n}\n",
			)],
		);
		let after = layer(
			&[],
			&[(
				"decisions/after.txt",
				"country_decisions = {\n\tfresh = { effect = { set_country_flag = after } }\n\tadded = { effect = { set_country_flag = added } }\n}\n",
			)],
		);
		let decisions = overlay_decisions(&[vanilla, resetting, after]).unwrap();
		assert_eq!(names(&decisions), ["fresh", "added"]);
		let fresh = decisions
			.iter()
			.find(|decision| decision.name == "fresh")
			.unwrap();
		assert!(fresh.body.contains("set_country_flag = after"));
	}

	#[test]
	fn replacing_a_decisions_file_drops_names_that_existed_only_there() {
		let vanilla = layer(
			&[],
			&[(
				"decisions/00.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = vanilla } }\n\tonly_there = { effect = { set_country_flag = gone } }\n}\n",
			)],
		);
		let revision = layer(
			&[],
			&[(
				"decisions/00.txt",
				"country_decisions = {\n\tkeep = { effect = { set_country_flag = revised } }\n}\n",
			)],
		);
		let decisions = overlay_decisions(&[vanilla, revision]).unwrap();
		assert_eq!(names(&decisions), ["keep"]);
		assert!(decisions[0].body.contains("set_country_flag = revised"));
	}

	#[test]
	fn nested_country_decisions_are_the_only_definitions() {
		let layer = layer(
			&[],
			&[(
				"decisions/00.txt",
				"not_a_decision = { effect = { set_country_flag = no } }\ncountry_decisions = {\n\treal = { effect = { set_country_flag = yes } }\n}\ncountry_decisions = {\n\treal = { effect = { set_country_flag = second } }\n}\n",
			)],
		);
		let decisions = overlay_decisions(&[layer]).unwrap();
		assert_eq!(names(&decisions), ["real"]);
		assert!(decisions[0].body.contains("set_country_flag = second"));
	}

	#[test]
	fn history_follows_the_case_date_and_does_not_assume_a_vanilla_list() {
		let vanilla = layer(
			&[],
			&[
				(
					"history/countries/SWE - Sweden.txt",
					"religion = catholic\nprimary_culture = swedish\ngovernment = monarchy\ngovernment_rank = 1\n1500.1.1 = { religion = protestant government_rank = 2 }\n",
				),
				(
					"history/provinces/1-Uppland.txt",
					"owner = SWE\ncontroller = SWE\nculture = swedish\nreligion = catholic\n1500.1.1 = { owner = DAN }\n",
				),
			],
		);
		let overhaul = layer(
			&[],
			&[
				(
					"history/countries/Q01 - Custom.txt",
					"religion = norse\nprimary_culture = custom\n",
				),
				(
					"history/provinces/9000 - Newland.txt",
					"owner = Q01\nreligion = norse\n1500.1.1 = { religion = catholic }\n",
				),
			],
		);
		let timeline = overlay_history(&[vanilla, overhaul]).unwrap();
		let tags: BTreeSet<_> = timeline
			.countries
			.keys()
			.map(|tag| tag.as_str().to_string())
			.collect();
		assert_eq!(tags, BTreeSet::from(["SWE".into(), "Q01".into()]));
		assert!(timeline.provinces.contains_key(&9000));
		assert!(timeline.provinces.contains_key(&1));

		let early = fold_history(&timeline, GameDate::parse("1444.11.11").unwrap());
		assert_eq!(
			early.countries[&Tag::parse("SWE").unwrap()]
				.religion
				.as_deref(),
			Some("catholic")
		);
		assert_eq!(
			early.countries[&Tag::parse("SWE").unwrap()].government_rank,
			Some(1)
		);
		assert_eq!(
			early.countries[&Tag::parse("Q01").unwrap()]
				.religion
				.as_deref(),
			Some("norse")
		);
		assert_eq!(
			early.provinces[&1].owner.as_ref().map(Tag::as_str),
			Some("SWE")
		);
		assert_eq!(early.provinces[&9000].religion.as_deref(), Some("norse"));

		let later = fold_history(&timeline, GameDate::parse("1500.1.1").unwrap());
		assert_eq!(
			later.countries[&Tag::parse("SWE").unwrap()]
				.religion
				.as_deref(),
			Some("protestant")
		);
		assert_eq!(
			later.countries[&Tag::parse("SWE").unwrap()].government_rank,
			Some(2)
		);
		assert_eq!(
			later.countries[&Tag::parse("SWE").unwrap()]
				.culture
				.as_deref(),
			Some("swedish")
		);
		assert_eq!(
			later.provinces[&1].owner.as_ref().map(Tag::as_str),
			Some("DAN")
		);
		assert_eq!(later.provinces[&9000].religion.as_deref(), Some("catholic"));
		assert_eq!(
			later.provinces[&9000].owner.as_ref().map(Tag::as_str),
			Some("Q01")
		);
	}

	#[test]
	fn a_replaced_history_file_does_not_keep_the_earlier_date_blocks() {
		let vanilla = layer(
			&[],
			&[(
				"history/countries/SWE - Sweden.txt",
				"religion = catholic\n1500.1.1 = { religion = protestant }\n",
			)],
		);
		let revision = layer(
			&[],
			&[(
				"history/countries/SWE - Sweden.txt",
				"religion = reformed\n",
			)],
		);
		let timeline = overlay_history(&[vanilla, revision]).unwrap();
		let at = fold_history(&timeline, GameDate::parse("1600.1.1").unwrap());
		assert_eq!(
			at.countries[&Tag::parse("SWE").unwrap()]
				.religion
				.as_deref(),
			Some("reformed")
		);
	}

	#[test]
	fn replace_path_clears_only_the_named_history_directory() {
		let vanilla = layer(
			&[],
			&[
				(
					"history/countries/SWE - Sweden.txt",
					"religion = catholic\n",
				),
				("history/provinces/12 - Bergslagen.txt", "owner = SWE\n"),
			],
		);
		let countries_only = layer(
			&["history/countries"],
			&[("history/countries/ZZZ - New.txt", "religion = jewish\n")],
		);
		let timeline = overlay_history(&[vanilla.clone(), countries_only]).unwrap();
		let tags: Vec<_> = timeline.countries.keys().map(|tag| tag.as_str()).collect();
		assert_eq!(tags, ["ZZZ"]);
		assert_eq!(
			timeline.provinces[&12]
				.first()
				.and_then(|slice| slice.owner.as_ref())
				.map(Tag::as_str),
			Some("SWE"),
			"history/countries does not reset history/provinces"
		);

		let whole = layer(&["history"], &[]);
		let cleared = overlay_history(&[vanilla, whole]).unwrap();
		assert!(cleared.countries.is_empty());
		assert!(cleared.provinces.is_empty());
	}

	#[test]
	fn a_date_block_after_the_case_date_does_not_apply() {
		let vanilla = layer(
			&[],
			&[(
				"history/countries/SWE - Sweden.txt",
				"1500.1.1 = { religion = protestant }\n",
			)],
		);
		let timeline = overlay_history(&[vanilla]).unwrap();
		let early = fold_history(&timeline, GameDate::parse("1444.11.11").unwrap());
		assert_eq!(early.countries[&Tag::parse("SWE").unwrap()].religion, None);
		assert!(early.tags.contains(&Tag::parse("SWE").unwrap()));
	}
}
