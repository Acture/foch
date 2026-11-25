//! Slider model for the out-of-process tribute panel.
//!
//! The window is a separate Windows tool (`tribute-panel`). It never injects
//! into the game. Saves land in `tribute-config.json`, which the plugin
//! hot-reloads.
//!
//! The cdylib does not count the panel binary as a caller, so the model is
//! allowed to look unused in that build.

#![cfg_attr(not(test), allow(dead_code))]

use crate::coefficient::TributeConfig;
use crate::subject_types::SubjectTypeGraph;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

/// One subject type in `copy_from` tree order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRow {
	pub key: String,
	pub depth: u8,
	/// Percent written for this key. `None` means the row still inherits.
	pub explicit_percent: Option<i64>,
	/// What the hot path would use for this key before a tag override.
	pub effective_percent: i64,
}

/// Tag override shown next to the type tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagRow {
	pub tag: String,
	pub percent: i64,
}

/// The panel's editable state. Sliders write explicit percents; untouched
/// types keep inheriting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliderModel {
	pub enabled: bool,
	pub global_percent: i64,
	pub types: Vec<TypeRow>,
	pub tags: Vec<TagRow>,
}

impl SliderModel {
	pub fn from_parts(graph: &SubjectTypeGraph, config: &TributeConfig) -> Self {
		let mut model = Self {
			enabled: config.enabled,
			global_percent: clamp(config.global_percent),
			types: tree_rows(graph, config),
			tags: config
				.tag_percent
				.iter()
				.map(|(tag, percent)| TagRow {
					tag: tag.clone(),
					percent: clamp(*percent),
				})
				.collect(),
		};
		model.refresh_effective(graph);
		model
	}

	pub fn set_enabled(&mut self, enabled: bool) {
		self.enabled = enabled;
	}

	pub fn set_global(&mut self, percent: i64, graph: &SubjectTypeGraph) {
		self.global_percent = clamp(percent);
		self.refresh_effective(graph);
	}

	pub fn set_type_percent(&mut self, key: &str, percent: i64, graph: &SubjectTypeGraph) {
		if let Some(row) = self.types.iter_mut().find(|row| row.key == key) {
			row.explicit_percent = Some(clamp(percent));
		}
		self.refresh_effective(graph);
	}

	pub fn set_tag_percent(&mut self, tag: &str, percent: i64) {
		let tag = tag.trim().to_ascii_uppercase();
		if tag.len() != 3 || !tag.chars().all(|ch| ch.is_ascii_alphanumeric()) {
			return;
		}
		let percent = clamp(percent);
		if let Some(row) = self.tags.iter_mut().find(|row| row.tag == tag) {
			row.percent = percent;
		} else {
			self.tags.push(TagRow { tag, percent });
			self.tags.sort_by(|left, right| left.tag.cmp(&right.tag));
		}
	}

	pub fn to_config(&self) -> TributeConfig {
		let mut kind_percent = BTreeMap::new();
		for row in &self.types {
			if let Some(percent) = row.explicit_percent {
				kind_percent.insert(row.key.clone(), percent);
			}
		}
		let mut tag_percent = BTreeMap::new();
		for row in &self.tags {
			tag_percent.insert(row.tag.clone(), row.percent);
		}
		TributeConfig {
			enabled: self.enabled,
			global_percent: self.global_percent,
			kind_percent,
			tag_percent,
		}
	}

	pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
		self.to_config().write_data_file(data_dir)
	}

	fn refresh_effective(&mut self, graph: &SubjectTypeGraph) {
		let config = self.to_config();
		for row in &mut self.types {
			let scale = config.coefficient("", &row.key, graph);
			row.effective_percent = clamp((scale * 100.0).round() as i64);
		}
	}
}

fn clamp(percent: i64) -> i64 {
	percent.clamp(0, 200)
}

fn tree_rows(graph: &SubjectTypeGraph, config: &TributeConfig) -> Vec<TypeRow> {
	let mut known: BTreeSet<String> = graph.keys().map(str::to_string).collect();
	known.extend(config.kind_percent.keys().cloned());
	let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
	let mut roots = Vec::new();
	for key in &known {
		match graph.parent(key) {
			Some(parent) if parent != key && known.contains(parent) => {
				children
					.entry(parent.to_string())
					.or_default()
					.push(key.clone());
			}
			_ => roots.push(key.clone()),
		}
	}
	for kids in children.values_mut() {
		kids.sort();
	}
	roots.sort();
	let mut rows = Vec::new();
	let mut placed = HashSet::new();
	for root in &roots {
		walk(
			root,
			0,
			config,
			&children,
			&mut Vec::new(),
			&mut placed,
			&mut rows,
		);
	}
	let mut leftover: Vec<String> = known
		.into_iter()
		.filter(|key| !placed.contains(key))
		.collect();
	leftover.sort();
	for key in leftover {
		walk(
			&key,
			0,
			config,
			&children,
			&mut Vec::new(),
			&mut placed,
			&mut rows,
		);
	}
	rows
}

fn walk(
	key: &str,
	depth: u8,
	config: &TributeConfig,
	children: &BTreeMap<String, Vec<String>>,
	stack: &mut Vec<String>,
	placed: &mut HashSet<String>,
	rows: &mut Vec<TypeRow>,
) {
	if stack.iter().any(|seen| seen == key) || !placed.insert(key.to_string()) {
		return;
	}
	rows.push(TypeRow {
		key: key.to_string(),
		depth,
		explicit_percent: config.kind_percent.get(key).copied(),
		effective_percent: clamp(config.global_percent),
	});
	stack.push(key.to_string());
	if let Some(kids) = children.get(key) {
		for child in kids {
			let next = depth.saturating_add(1);
			walk(child, next, config, children, stack, placed, rows);
		}
	}
	stack.pop();
}

#[cfg(test)]
mod tests {
	use super::*;

	fn effective(model: &SliderModel, key: &str) -> i64 {
		model
			.types
			.iter()
			.find(|row| row.key == key)
			.unwrap()
			.effective_percent
	}

	fn chain() -> SubjectTypeGraph {
		SubjectTypeGraph::from_script(
			"\
default = {}
vassal = { copy_from = default }
eyalet = { copy_from = vassal }
core_eyalet = { copy_from = eyalet }
beylerbey = { copy_from = core_eyalet }
",
		)
		.unwrap()
	}

	#[test]
	fn the_tree_follows_copy_from_including_a_mod_type() {
		let graph = chain();
		let mut config = TributeConfig {
			global_percent: 80,
			..TributeConfig::default()
		};
		config.kind_percent.insert("vassal".into(), 50);
		let model = SliderModel::from_parts(&graph, &config);
		let keys: Vec<_> = model
			.types
			.iter()
			.map(|row| (row.depth, row.key.as_str(), row.effective_percent))
			.collect();
		assert_eq!(
			keys,
			vec![
				(0, "default", 80),
				(1, "vassal", 50),
				(2, "eyalet", 50),
				(3, "core_eyalet", 50),
				(4, "beylerbey", 50),
			]
		);
		assert_eq!(
			model
				.types
				.iter()
				.find(|row| row.key == "vassal")
				.unwrap()
				.explicit_percent,
			Some(50)
		);
		assert!(
			model
				.types
				.iter()
				.find(|row| row.key == "core_eyalet")
				.unwrap()
				.explicit_percent
				.is_none()
		);
	}

	#[test]
	fn editing_a_parent_updates_descendants_until_they_are_explicit() {
		let graph = chain();
		let config = TributeConfig::default();
		let mut model = SliderModel::from_parts(&graph, &config);
		model.set_type_percent("eyalet", 30, &graph);
		assert_eq!(effective(&model, "eyalet"), 30);
		assert_eq!(effective(&model, "core_eyalet"), 30);
		assert_eq!(effective(&model, "beylerbey"), 30);
		assert_eq!(effective(&model, "vassal"), 100);
		model.set_type_percent("core_eyalet", 70, &graph);
		assert_eq!(effective(&model, "core_eyalet"), 70);
		assert_eq!(effective(&model, "beylerbey"), 70);
		assert_eq!(effective(&model, "eyalet"), 30);
	}

	#[test]
	fn a_cycle_is_listed_once() {
		let graph = SubjectTypeGraph::from_script(
			"a = { copy_from = b }\nb = { copy_from = a }\ncustom = { copy_from = a }\n",
		)
		.unwrap();
		let model = SliderModel::from_parts(&graph, &TributeConfig::default());
		let mut keys: Vec<_> = model.types.iter().map(|row| row.key.as_str()).collect();
		keys.sort();
		assert_eq!(keys, vec!["a", "b", "custom"]);
	}

	#[test]
	fn save_writes_only_explicit_percents_for_the_plugin_to_reload() {
		let dir = tempfile::tempdir().unwrap();
		let graph = chain();
		let mut watch =
			crate::config::ConfigWatch::new(dir.path().to_path_buf(), TributeConfig::default());
		let mut model = SliderModel::from_parts(&graph, &TributeConfig::default());
		model.set_enabled(false);
		model.set_global(90, &graph);
		model.set_type_percent("vassal", 40, &graph);
		model.set_tag_percent("wls", 15);
		model.set_tag_percent("nope", 10);
		model.save(dir.path()).unwrap();
		let loaded = watch.poll(1_000).expect("the save is a new stamp");
		assert!(!loaded.enabled);
		assert_eq!(loaded.global_percent, 90);
		assert_eq!(loaded.kind_percent.get("vassal"), Some(&40));
		assert!(!loaded.kind_percent.contains_key("eyalet"));
		assert_eq!(loaded.tag_percent.get("WLS"), Some(&15));
		assert!(!loaded.tag_percent.contains_key("NOPE"));
		let text = std::fs::read_to_string(dir.path().join(crate::config::DATA_FILE)).unwrap();
		assert!(text.contains("vassal"));
	}
}
