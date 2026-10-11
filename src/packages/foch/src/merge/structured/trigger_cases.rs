//! A trigger `if`/`else` chain is a pure condition: it holds when the body of
//! the branch whose guard holds does. When one mod replaces such a chain with
//! plain conditions and another edits the chain's branches, the tree kernel
//! sees a delete against a modify. Branch by branch, though, both mods made an
//! ordinary edit to the same condition list: the replacing mod gave every
//! branch the same body. Merging each branch on its own and collapsing
//! branches that come out identical is therefore exact, and it keeps both
//! mods' intent where the kernel could only report the conflict.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::game::eu4::content::ScriptFileKind;
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue};
use crate::game::eu4::script::{classify_script_file, script_container_scope_kind};
use crate::model::{GamePath, ScopeKind};

use super::ast_adapter::{AstAdapterError, synthetic_span};
use super::merge::{ScriptContext, script_context};

/// Merge one branch: the base body and each revision's body, wrapped at the
/// chain's block path. `None` when that merge has a conflict.
pub(super) type BranchMerge<'a> =
	dyn FnMut(&AstFile, &[&AstFile]) -> Result<Option<AstFile>, AstAdapterError> + 'a;

/// Rewrite the revisions so that every trigger chain one revision replaced and
/// another edited carries its branch-wise merge in all of them. Returns `None`
/// when nothing was rewritten.
pub(super) fn resolve_replaced_trigger_chains(
	base: &AstFile,
	revisions: &[&AstFile],
	merge_branch: &mut BranchMerge<'_>,
) -> Result<Option<Vec<AstFile>>, AstAdapterError> {
	let mut walk = Walk {
		file_path: &base.path,
		file_kind: classify_script_file(&base.path),
		scope_cache: HashMap::new(),
		revisions: revisions.iter().map(|file| Cow::Borrowed(*file)).collect(),
		rewritten: false,
	};
	walk.statements(
		&base.statements,
		&mut Vec::new(),
		ScriptContext::Data,
		merge_branch,
	)?;
	Ok(walk
		.rewritten
		.then(|| walk.revisions.into_iter().map(Cow::into_owned).collect()))
}

struct Walk<'a> {
	file_path: &'a GamePath,
	file_kind: ScriptFileKind,
	scope_cache: HashMap<Vec<String>, Option<ScopeKind>>,
	revisions: Vec<Cow<'a, AstFile>>,
	rewritten: bool,
}

impl Walk<'_> {
	fn statements(
		&mut self,
		statements: &[AstStatement],
		path: &mut Vec<String>,
		parent_context: ScriptContext,
		merge_branch: &mut BranchMerge<'_>,
	) -> Result<(), AstAdapterError> {
		for statement in statements {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				continue;
			};
			// A path names one block only when its every key is unique.
			if block_count(statements, key) != 1 {
				continue;
			}
			path.push(key.clone());
			let scope_kind = *self.scope_cache.entry(path.clone()).or_insert_with(|| {
				let refs = path.iter().map(String::as_str).collect::<Vec<_>>();
				script_container_scope_kind(self.file_kind.clone(), self.file_path, &refs)
			});
			let context = script_context(parent_context, key, scope_kind);
			if context == ScriptContext::Trigger {
				for chain in complete_chains(items) {
					self.resolve_chain(items, &chain, path, merge_branch)?;
				}
			}
			self.statements(items, path, context, merge_branch)?;
			path.pop();
		}
		Ok(())
	}

	fn resolve_chain(
		&mut self,
		base_items: &[AstStatement],
		chain: &Chain,
		path: &[String],
		merge_branch: &mut BranchMerge<'_>,
	) -> Result<(), AstAdapterError> {
		let base_branches = branches(&base_items[chain.start..chain.end]);
		let base_guards = guards(&base_branches);
		let mut shapes = Vec::with_capacity(self.revisions.len());
		for revision in &self.revisions {
			let Some(items) = block_at(&revision.statements, path) else {
				return Ok(());
			};
			let kept = complete_chains(items).into_iter().find(|candidate| {
				guards(&branches(&items[candidate.start..candidate.end])) == base_guards
			});
			shapes.push(match kept {
				Some(kept) => Shape::Kept {
					bodies: branches(&items[kept.start..kept.end])
						.into_iter()
						.map(|branch| branch.body)
						.collect(),
					range: kept.start..kept.end,
				},
				None => match replacement_range(base_items, chain, items) {
					Some(range) => Shape::Replaced {
						body: significant(&items[range.clone()]),
						range,
					},
					None => return Ok(()),
				},
			});
		}
		let replaced = shapes
			.iter()
			.any(|shape| matches!(shape, Shape::Replaced { .. }));
		let edited = shapes.iter().any(|shape| match shape {
			Shape::Kept { bodies, .. } => bodies
				.iter()
				.zip(&base_branches)
				.any(|(body, base)| text(body) != text(&base.body)),
			Shape::Replaced { .. } => false,
		});
		if !replaced || !edited {
			return Ok(());
		}

		let mut merged = Vec::with_capacity(base_branches.len());
		for (index, base_branch) in base_branches.iter().enumerate() {
			let base_file = nested(self.file_path, path, base_branch.body.clone());
			let revision_files = shapes
				.iter()
				.map(|shape| {
					let body = match shape {
						Shape::Kept { bodies, .. } => bodies[index].clone(),
						Shape::Replaced { body, .. } => body.clone(),
					};
					nested(self.file_path, path, body)
				})
				.collect::<Vec<_>>();
			let revision_refs = revision_files.iter().collect::<Vec<_>>();
			let Some(output) = merge_branch(&base_file, &revision_refs)? else {
				return Ok(());
			};
			let Some(body) = block_at(&output.statements, path) else {
				return Ok(());
			};
			merged.push(significant(body));
		}
		let replacement = if merged
			.windows(2)
			.all(|pair| text(&pair[0]) == text(&pair[1]))
		{
			merged.swap_remove(0)
		} else {
			base_branches
				.iter()
				.zip(merged)
				.map(|(branch, body)| branch.rebuild(body))
				.collect()
		};

		for (revision, shape) in self.revisions.iter_mut().zip(shapes) {
			let items = block_at_mut(&mut revision.to_mut().statements, path)
				.expect("the block was found before the rewrite");
			let range = match shape {
				Shape::Kept { range, .. } | Shape::Replaced { range, .. } => range,
			};
			items.splice(range, replacement.iter().cloned());
		}
		self.rewritten = true;
		Ok(())
	}
}

enum Shape {
	/// The revision keeps a chain with the base guards.
	Kept {
		bodies: Vec<Vec<AstStatement>>,
		range: std::ops::Range<usize>,
	},
	/// The revision replaced the chain with the statements in `range`.
	Replaced {
		body: Vec<AstStatement>,
		range: std::ops::Range<usize>,
	},
}

#[derive(Clone, Debug)]
struct Chain {
	start: usize,
	end: usize,
}

struct Branch {
	key: String,
	limit: Option<AstStatement>,
	body: Vec<AstStatement>,
}

impl Branch {
	fn rebuild(&self, body: Vec<AstStatement>) -> AstStatement {
		let items = self.limit.iter().cloned().chain(body).collect();
		assignment(&self.key, items)
	}
}

/// Every `if`, `else_if`* , `else` run in `items`. Open chains are skipped: a
/// missing `else` means "no condition" there, not a body to merge.
fn complete_chains(items: &[AstStatement]) -> Vec<Chain> {
	let mut chains = Vec::new();
	let mut index = 0;
	while index < items.len() {
		if key_of(&items[index]) != Some("if") {
			index += 1;
			continue;
		}
		let start = index;
		let mut cursor = index + 1;
		let mut complete = false;
		loop {
			let mut next = cursor;
			while matches!(items.get(next), Some(AstStatement::Comment { .. })) {
				next += 1;
			}
			match items.get(next).and_then(key_of) {
				Some("else_if") => cursor = next + 1,
				Some("else") => {
					cursor = next + 1;
					complete = true;
					break;
				}
				_ => break,
			}
		}
		if complete {
			chains.push(Chain { start, end: cursor });
		}
		index = cursor;
	}
	chains
}

fn branches(statements: &[AstStatement]) -> Vec<Branch> {
	statements
		.iter()
		.filter_map(|statement| {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				return None;
			};
			let limit = items
				.iter()
				.find(|item| key_of(item) == Some("limit"))
				.cloned();
			let body = significant(items)
				.into_iter()
				.filter(|item| key_of(item) != Some("limit"))
				.collect();
			Some(Branch {
				key: key.clone(),
				limit,
				body,
			})
		})
		.collect()
}

fn guards(branches: &[Branch]) -> Vec<(String, Option<String>)> {
	branches
		.iter()
		.map(|branch| {
			(
				branch.key.clone(),
				branch
					.limit
					.as_ref()
					.and_then(|limit| text(std::slice::from_ref(limit))),
			)
		})
		.collect()
}

/// The statements a revision put where the base chain stood: everything
/// between the chain's surviving neighbours.
fn replacement_range(
	base_items: &[AstStatement],
	chain: &Chain,
	items: &[AstStatement],
) -> Option<std::ops::Range<usize>> {
	let before = base_items[..chain.start]
		.iter()
		.rev()
		.find(|item| !matches!(item, AstStatement::Comment { .. }));
	let after = base_items[chain.end..]
		.iter()
		.find(|item| !matches!(item, AstStatement::Comment { .. }));
	let start = match before {
		Some(before) => unique_position(items, before)? + 1,
		None => 0,
	};
	let end = match after {
		Some(after) => unique_position(items, after)?,
		None => items.len(),
	};
	(start <= end).then_some(start..end)
}

fn unique_position(items: &[AstStatement], wanted: &AstStatement) -> Option<usize> {
	let wanted = text(std::slice::from_ref(wanted))?;
	let mut found = items
		.iter()
		.enumerate()
		.filter(|(_, item)| text(std::slice::from_ref(*item)).as_deref() == Some(&wanted))
		.map(|(index, _)| index);
	let first = found.next()?;
	found.next().is_none().then_some(first)
}

fn block_count(statements: &[AstStatement], key: &str) -> usize {
	statements
		.iter()
		.filter(|statement| key_of(statement) == Some(key))
		.count()
}

fn block_at<'a>(statements: &'a [AstStatement], path: &[String]) -> Option<&'a [AstStatement]> {
	let (first, rest) = path.split_first()?;
	if block_count(statements, first) != 1 {
		return None;
	}
	let items = statements.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == first => Some(items.as_slice()),
		_ => None,
	})?;
	if rest.is_empty() {
		Some(items)
	} else {
		block_at(items, rest)
	}
}

fn block_at_mut<'a>(
	statements: &'a mut [AstStatement],
	path: &[String],
) -> Option<&'a mut Vec<AstStatement>> {
	let (first, rest) = path.split_first()?;
	let items = statements
		.iter_mut()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == first => Some(items),
			_ => None,
		})?;
	if rest.is_empty() {
		Some(items)
	} else {
		block_at_mut(items, rest)
	}
}

fn nested(file_path: &GamePath, path: &[String], body: Vec<AstStatement>) -> AstFile {
	let statements = path
		.iter()
		.rev()
		.fold(body, |items, key| vec![assignment(key, items)]);
	AstFile {
		path: file_path.to_owned(),
		statements,
	}
}

fn assignment(key: &str, items: Vec<AstStatement>) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Block {
			items,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn key_of(statement: &AstStatement) -> Option<&str> {
	match statement {
		AstStatement::Assignment { key, .. } => Some(key),
		_ => None,
	}
}

/// The statements without comments, which carry no condition.
fn significant(statements: &[AstStatement]) -> Vec<AstStatement> {
	statements
		.iter()
		.filter(|statement| !matches!(statement, AstStatement::Comment { .. }))
		.map(strip_comments)
		.collect()
}

fn strip_comments(statement: &AstStatement) -> AstStatement {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block { items, span },
			span: statement_span,
		} => AstStatement::Assignment {
			key: key.clone(),
			key_span: key_span.clone(),
			value: AstValue::Block {
				items: significant(items),
				span: span.clone(),
			},
			span: statement_span.clone(),
		},
		AstStatement::Item {
			value: AstValue::Block { items, span },
			span: statement_span,
		} => AstStatement::Item {
			value: AstValue::Block {
				items: significant(items),
				span: span.clone(),
			},
			span: statement_span.clone(),
		},
		other => other.clone(),
	}
}

fn text(statements: &[AstStatement]) -> Option<String> {
	emit_clausewitz_statements(&significant(statements)).ok()
}
