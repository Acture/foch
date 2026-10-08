//! Choosing cases: node-ID prefixes, `-k` keyword and `-m` mark expressions.

use crate::model::{NodeId, TestCase};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A boolean expression over words: `reform and not (slow or war)`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expr {
	Word(String),
	Not(Box<Expr>),
	And(Box<Expr>, Box<Expr>),
	Or(Box<Expr>, Box<Expr>),
}

impl Expr {
	pub fn parse(text: &str) -> Result<Self, String> {
		let tokens = tokenize(text);
		let mut position = 0;
		let expr = parse_or(&tokens, &mut position)?;
		if position != tokens.len() {
			return Err(format!(
				"unexpected {:?} in expression {text:?}",
				tokens[position]
			));
		}
		Ok(expr)
	}

	pub fn eval(&self, word: &dyn Fn(&str) -> bool) -> bool {
		match self {
			Self::Word(value) => word(value),
			Self::Not(inner) => !inner.eval(word),
			Self::And(left, right) => left.eval(word) && right.eval(word),
			Self::Or(left, right) => left.eval(word) || right.eval(word),
		}
	}
}

fn tokenize(text: &str) -> Vec<String> {
	let mut tokens = Vec::new();
	let mut current = String::new();
	for character in text.chars() {
		if character.is_whitespace() || matches!(character, '(' | ')') {
			if !current.is_empty() {
				tokens.push(std::mem::take(&mut current));
			}
			if !character.is_whitespace() {
				tokens.push(character.to_string());
			}
		} else {
			current.push(character);
		}
	}
	if !current.is_empty() {
		tokens.push(current);
	}
	tokens
}

fn parse_or(tokens: &[String], position: &mut usize) -> Result<Expr, String> {
	let mut left = parse_and(tokens, position)?;
	while tokens.get(*position).is_some_and(|token| token == "or") {
		*position += 1;
		left = Expr::Or(Box::new(left), Box::new(parse_and(tokens, position)?));
	}
	Ok(left)
}

fn parse_and(tokens: &[String], position: &mut usize) -> Result<Expr, String> {
	let mut left = parse_not(tokens, position)?;
	while tokens.get(*position).is_some_and(|token| token == "and") {
		*position += 1;
		left = Expr::And(Box::new(left), Box::new(parse_not(tokens, position)?));
	}
	Ok(left)
}

fn parse_not(tokens: &[String], position: &mut usize) -> Result<Expr, String> {
	match tokens.get(*position).map(String::as_str) {
		Some("not") => {
			*position += 1;
			Ok(Expr::Not(Box::new(parse_not(tokens, position)?)))
		}
		Some("(") => {
			*position += 1;
			let inner = parse_or(tokens, position)?;
			if tokens.get(*position).map(String::as_str) != Some(")") {
				return Err("missing ) in expression".into());
			}
			*position += 1;
			Ok(inner)
		}
		Some(")" | "and" | "or") | None => Err("expected a word in expression".into()),
		Some(word) => {
			*position += 1;
			Ok(Expr::Word(word.into()))
		}
	}
}

/// Matches a node ID or one of its prefixes at a segment boundary, with or
/// without the leading mod name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeIdPattern(String);

impl NodeIdPattern {
	pub fn parse(text: &str) -> Result<Self, String> {
		let text = text.trim().replace('\\', "/");
		if text.is_empty() {
			return Err("empty node ID pattern".into());
		}
		Ok(Self(text))
	}

	pub fn matches(&self, node: &NodeId) -> bool {
		[node.to_string(), node.local()].iter().any(|candidate| {
			candidate.strip_prefix(self.0.as_str()).is_some_and(|rest| {
				rest.is_empty() || rest.starts_with("::") || rest.starts_with('[')
			})
		})
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Selector {
	/// Empty selects every case.
	pub nodes: Vec<NodeIdPattern>,
	/// pytest `-k`: words match case-insensitively anywhere in the node ID.
	pub keyword: Option<Expr>,
	/// pytest `-m`: words match labels given with `#mark`.
	pub marks: Option<Expr>,
	/// Node IDs to keep, such as the failures of a previous run.
	pub only: Option<BTreeSet<String>>,
}

impl Selector {
	pub fn matches(&self, case: &TestCase) -> bool {
		let node = case.node.to_string();
		(self.nodes.is_empty() || self.nodes.iter().any(|pattern| pattern.matches(&case.node)))
			&& self.keyword.as_ref().is_none_or(|expr| {
				let haystack = node.to_lowercase();
				expr.eval(&|word| haystack.contains(&word.to_lowercase()))
			}) && self
			.marks
			.as_ref()
			.is_none_or(|expr| expr.eval(&|word| case.marks.labels.contains(word)))
			&& self.only.as_ref().is_none_or(|only| only.contains(&node))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn expressions_follow_not_and_or_precedence() {
		let expr = Expr::parse("a or b and not c").unwrap();
		let truth = |set: &[&str]| expr.eval(&|word| set.contains(&word));
		assert!(truth(&["a"]));
		assert!(truth(&["b"]));
		assert!(!truth(&["b", "c"]));
		assert!(
			!Expr::parse("(a or b) and c")
				.unwrap()
				.eval(&|word| word == "a")
		);
		for bad in ["", "a and", "(a", "a b", "not"] {
			assert!(Expr::parse(bad).is_err(), "{bad:?}");
		}
	}

	#[test]
	fn node_patterns_match_whole_segments() {
		let node: NodeId = "m::events/a.txt::a.1::flag".parse().unwrap();
		for pattern in [
			"events/a.txt",
			"m::events/a.txt::a.1",
			"events/a.txt::a.1::flag",
		] {
			assert!(
				NodeIdPattern::parse(pattern).unwrap().matches(&node),
				"{pattern}"
			);
		}
		for pattern in ["events/a", "events/a.txt::a.10", "other::events/a.txt"] {
			assert!(
				!NodeIdPattern::parse(pattern).unwrap().matches(&node),
				"{pattern}"
			);
		}
	}
}
