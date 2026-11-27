//! Author-ordered playset completion.
//!
//! The caller supplies the mods an author listed, each with its position, and
//! a registry of mods that can fill a dependency that list left out. This
//! module does not read the disk. It never sorts by name. Position is the
//! only order, and a missing dependency is inserted immediately before the
//! mod that needs it, after that dependency's own missing dependencies.

use std::collections::{BTreeMap, BTreeSet};

/// One mod in an author's playset or in the registry that fills gaps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadMod {
	pub name: String,
	/// Other tokens that name this same mod, such as a launcher id.
	pub aliases: Vec<String>,
	/// Dependency tokens, in the order the descriptor lists them.
	pub dependencies: Vec<String>,
	/// Author position. Registry entries may leave this at zero.
	pub position: usize,
}

/// Sort `author` by position and insert missing dependencies in front of the
/// mod that declares them.
///
/// A dependency token that already names an author mod is not moved, and that
/// mod's own dependencies are filled only when the walk reaches it. The
/// returned names are the unresolved tokens, each once, in the order they
/// were first required.
pub fn complete_load_order(
	mut author: Vec<LoadMod>,
	registry: &[LoadMod],
) -> (Vec<LoadMod>, Vec<String>) {
	author.sort_by_key(|item| item.position);
	let mut author_tokens = BTreeSet::new();
	for item in &author {
		remember(&mut author_tokens, item);
	}
	let mut index: BTreeMap<&str, &LoadMod> = BTreeMap::new();
	for item in registry {
		index.entry(item.name.as_str()).or_insert(item);
		for alias in &item.aliases {
			index.entry(alias.as_str()).or_insert(item);
		}
	}
	let mut ordered = Vec::new();
	let mut placed = BTreeSet::new();
	let mut visited = BTreeSet::new();
	let mut unresolved = Vec::new();
	for item in &author {
		insert_missing(
			&item.dependencies,
			&index,
			&author_tokens,
			&mut visited,
			&mut placed,
			&mut unresolved,
			&mut ordered,
		);
		ordered.push(item.clone());
		remember(&mut placed, item);
	}
	(ordered, unresolved)
}

fn remember(tokens: &mut BTreeSet<String>, item: &LoadMod) {
	tokens.insert(item.name.clone());
	tokens.extend(item.aliases.iter().cloned());
}

fn known(item: &LoadMod, author_tokens: &BTreeSet<String>, placed: &BTreeSet<String>) -> bool {
	author_tokens.contains(&item.name)
		|| placed.contains(&item.name)
		|| item
			.aliases
			.iter()
			.any(|alias| author_tokens.contains(alias) || placed.contains(alias))
}

/// Post-order over missing dependencies: a dependency's own dependencies are
/// emitted before it, each mod once. `visited` breaks cycles for the whole
/// walk, the same way the test command's dependency walk does.
fn insert_missing(
	tokens: &[String],
	index: &BTreeMap<&str, &LoadMod>,
	author_tokens: &BTreeSet<String>,
	visited: &mut BTreeSet<String>,
	placed: &mut BTreeSet<String>,
	unresolved: &mut Vec<String>,
	ordered: &mut Vec<LoadMod>,
) {
	for token in tokens {
		if !visited.insert(token.clone()) {
			continue;
		}
		if author_tokens.contains(token) || placed.contains(token) {
			continue;
		}
		let Some(found) = index.get(token.as_str()).copied() else {
			unresolved.push(token.clone());
			continue;
		};
		if known(found, author_tokens, placed) {
			continue;
		}
		insert_missing(
			&found.dependencies,
			index,
			author_tokens,
			visited,
			placed,
			unresolved,
			ordered,
		);
		if known(found, author_tokens, placed) {
			continue;
		}
		remember(placed, found);
		ordered.push(found.clone());
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn item(name: &str, position: usize, dependencies: &[&str]) -> LoadMod {
		LoadMod {
			name: name.to_string(),
			aliases: Vec::new(),
			dependencies: dependencies.iter().map(|dep| (*dep).to_string()).collect(),
			position,
		}
	}

	fn names(mods: &[LoadMod]) -> Vec<&str> {
		mods.iter().map(|item| item.name.as_str()).collect()
	}

	#[test]
	fn author_order_is_position_not_name() {
		let (order, unresolved) =
			complete_load_order(vec![item("Zed", 0, &[]), item("Amy", 1, &[])], &[]);
		assert_eq!(names(&order), ["Zed", "Amy"]);
		assert!(unresolved.is_empty());

		let (order, _) = complete_load_order(
			vec![
				item("Third", 2, &[]),
				item("First", 0, &[]),
				item("Second", 1, &[]),
			],
			&[],
		);
		assert_eq!(names(&order), ["First", "Second", "Third"]);
	}

	#[test]
	fn a_listed_dependency_stays_where_the_author_put_it() {
		let author = vec![item("Leaf", 0, &["Base"]), item("Base", 1, &[])];
		let (order, unresolved) = complete_load_order(author, &[item("Base", 0, &["Deep"])]);
		assert_eq!(names(&order), ["Leaf", "Base"]);
		assert!(unresolved.is_empty());
	}

	#[test]
	fn missing_dependencies_insert_before_the_mod_that_needs_them() {
		let author = vec![item("Other", 0, &[]), item("Leaf", 1, &["Base"])];
		let (order, unresolved) = complete_load_order(author, &[item("Base", 0, &[])]);
		assert_eq!(names(&order), ["Other", "Base", "Leaf"]);
		assert!(unresolved.is_empty());
	}

	#[test]
	fn transitive_missing_dependencies_use_post_order() {
		let author = vec![item("C", 0, &[]), item("B", 1, &["A"])];
		let registry = vec![item("A", 0, &["D"]), item("D", 0, &[])];
		let (order, unresolved) = complete_load_order(author, &registry);
		assert_eq!(names(&order), ["C", "D", "A", "B"]);
		assert!(unresolved.is_empty());

		let author = vec![item("Leaf", 0, &["A", "B"])];
		let registry = vec![
			item("A", 0, &["Base"]),
			item("B", 0, &["Base"]),
			item("Base", 0, &[]),
		];
		let (order, _) = complete_load_order(author, &registry);
		assert_eq!(names(&order), ["Base", "A", "B", "Leaf"]);
	}

	#[test]
	fn an_author_mod_keeps_its_own_dependencies_until_it_is_reached() {
		let author = vec![item("Leaf", 0, &["Mid"]), item("Base", 1, &["Deep"])];
		let registry = vec![item("Mid", 0, &["Base"]), item("Deep", 0, &[])];
		let (order, unresolved) = complete_load_order(author, &registry);
		assert_eq!(names(&order), ["Mid", "Leaf", "Deep", "Base"]);
		assert!(unresolved.is_empty());
	}

	#[test]
	fn an_alias_inserts_the_mod_once_and_a_missing_name_is_not_invented() {
		let mut base = item("Base", 0, &[]);
		base.aliases.push("123".to_string());
		let (order, unresolved) = complete_load_order(vec![item("Leaf", 0, &["123"])], &[base]);
		assert_eq!(names(&order), ["Base", "Leaf"]);
		assert!(unresolved.is_empty());

		let (order, unresolved) = complete_load_order(vec![item("Leaf", 0, &["Mid"])], &[]);
		assert_eq!(names(&order), ["Leaf"]);
		assert_eq!(unresolved, ["Mid"]);
	}

	#[test]
	fn unresolved_and_cycles_are_reported_once() {
		let author = vec![item("Other", 0, &["Ghost"]), item("Leaf", 1, &["Ghost"])];
		let (order, unresolved) = complete_load_order(author, &[]);
		assert_eq!(names(&order), ["Other", "Leaf"]);
		assert_eq!(unresolved, ["Ghost"]);

		let author = vec![item("Leaf", 0, &["A"])];
		let registry = vec![item("A", 0, &["B"]), item("B", 0, &["A"])];
		let (order, unresolved) = complete_load_order(author, &registry);
		assert_eq!(names(&order), ["B", "A", "Leaf"]);
		assert!(unresolved.is_empty());
	}
}
