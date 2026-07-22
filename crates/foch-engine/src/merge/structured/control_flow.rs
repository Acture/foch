use std::collections::{BTreeMap, BTreeSet};

use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
use foch_merge_kernel::{
	ChildCardinality, ChildOrder, NodeId, NormalizedNode, NormalizedTree, SemanticKey, TreeNode,
};

use super::ast_adapter::{
	AstAdapterError, COMMENT_KIND, assignment_key, branch, denormalize_only_value_child,
	denormalize_statement, normalize_statement, normalize_value, synthetic_span,
};
use super::policy::ClausewitzTreePolicy;

const CHAIN_KIND_PREFIX: &str = "clausewitz.control_flow.chain:";
const GUARDED_BRANCH_KIND_PREFIX: &str = "clausewitz.control_flow.guarded_branch:";
const ELSE_BRANCH_KIND: &str = "clausewitz.control_flow.else_branch";
const MAX_DNF_TERMS: usize = 256;

type Term = BTreeMap<String, bool>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Formula {
	terms: BTreeSet<Term>,
}

impl Formula {
	fn false_value() -> Self {
		Self {
			terms: BTreeSet::new(),
		}
	}

	fn true_value() -> Self {
		Self {
			terms: BTreeSet::from([Term::new()]),
		}
	}

	fn literal(atom: String) -> Self {
		Self {
			terms: BTreeSet::from([Term::from([(atom, true)])]),
		}
	}

	fn or(&self, other: &Self) -> Result<Self, AstAdapterError> {
		let mut terms = self.terms.clone();
		terms.extend(other.terms.iter().cloned());
		if terms.len() > MAX_DNF_TERMS {
			return Err(formula_size_error());
		}
		Self::from_terms(terms)
	}

	fn and(&self, other: &Self) -> Result<Self, AstAdapterError> {
		let mut terms = BTreeSet::new();
		for left in &self.terms {
			for right in &other.terms {
				let mut merged = left.clone();
				let mut contradiction = false;
				for (atom, polarity) in right {
					if merged
						.insert(atom.clone(), *polarity)
						.is_some_and(|previous| previous != *polarity)
					{
						contradiction = true;
						break;
					}
				}
				if !contradiction {
					terms.insert(merged);
					if terms.len() > MAX_DNF_TERMS {
						return Err(formula_size_error());
					}
				}
			}
		}
		Self::from_terms(terms)
	}

	fn not(&self) -> Result<Self, AstAdapterError> {
		let mut result = Self::true_value();
		for term in &self.terms {
			let mut negated = Self::false_value();
			for (atom, polarity) in term {
				negated = negated.or(&Self {
					terms: BTreeSet::from([Term::from([(atom.clone(), !polarity)])]),
				})?;
			}
			result = result.and(&negated)?;
		}
		Ok(result)
	}

	fn from_terms(mut terms: BTreeSet<Term>) -> Result<Self, AstAdapterError> {
		loop {
			let snapshot = terms.iter().cloned().collect::<Vec<_>>();
			let mut replacement = None;
			'pair: for (left_index, left) in snapshot.iter().enumerate() {
				for right in snapshot.iter().skip(left_index + 1) {
					let opposites = left
						.iter()
						.filter(|(atom, polarity)| {
							right.get(*atom).is_some_and(|other| other != *polarity)
						})
						.map(|(atom, _)| atom)
						.collect::<Vec<_>>();
					if opposites.len() != 1 {
						continue;
					}
					let atom = opposites[0];
					let mut left_rest = left.clone();
					let mut right_rest = right.clone();
					left_rest.remove(atom);
					right_rest.remove(atom);
					if term_contains(&right_rest, &left_rest) {
						replacement = Some((right.clone(), right_rest));
						break 'pair;
					}
					if term_contains(&left_rest, &right_rest) {
						replacement = Some((left.clone(), left_rest));
						break 'pair;
					}
				}
			}
			if let Some((old, new)) = replacement {
				terms.remove(&old);
				terms.insert(new);
				continue;
			}

			let snapshot = terms.iter().cloned().collect::<Vec<_>>();
			let redundant = snapshot.iter().find_map(|candidate| {
				snapshot
					.iter()
					.any(|other| candidate != other && term_contains(candidate, other))
					.then_some(candidate.clone())
			});
			if let Some(redundant) = redundant {
				terms.remove(&redundant);
				continue;
			}
			break;
		}
		if terms.len() > MAX_DNF_TERMS {
			return Err(formula_size_error());
		}
		Ok(Self { terms })
	}

	fn key(&self) -> String {
		self.terms
			.iter()
			.map(|term| {
				term.iter()
					.map(|(atom, polarity)| format!("{}{}", if *polarity { "" } else { "!" }, atom))
					.collect::<Vec<_>>()
					.join("&")
			})
			.collect::<Vec<_>>()
			.join("|")
	}

	fn is_false(&self) -> bool {
		self.terms.is_empty()
	}

	fn is_negative_only(&self) -> bool {
		!self.is_false()
			&& self
				.terms
				.iter()
				.all(|term| term.values().all(|polarity| !polarity))
	}

	fn positive_atoms(&self) -> BTreeSet<String> {
		self.terms
			.iter()
			.flat_map(|term| {
				term.iter()
					.filter(|(_, polarity)| **polarity)
					.map(|(atom, _)| atom.clone())
			})
			.collect()
	}

	fn common_negative_atoms(&self) -> BTreeSet<String> {
		let mut terms = self.terms.iter();
		let Some(first) = terms.next() else {
			return BTreeSet::new();
		};
		let mut common = first
			.iter()
			.filter(|(_, polarity)| !**polarity)
			.map(|(atom, _)| atom.clone())
			.collect::<BTreeSet<_>>();
		for term in terms {
			common.retain(|atom| term.get(atom) == Some(&false));
		}
		common
	}

	fn without_prior_negations(
		&self,
		prior_positive_atoms: &BTreeSet<String>,
	) -> Result<Self, AstAdapterError> {
		Self::from_terms(
			self.terms
				.iter()
				.map(|term| {
					term.iter()
						.filter(|(atom, polarity)| {
							**polarity || !prior_positive_atoms.contains(*atom)
						})
						.map(|(atom, polarity)| (atom.clone(), *polarity))
						.collect()
				})
				.collect(),
		)
	}
}

fn formula_size_error() -> AstAdapterError {
	AstAdapterError::UnprovableControlFlow(format!(
		"guard normalization exceeded {MAX_DNF_TERMS} disjuncts"
	))
}

fn term_contains(term: &Term, subset: &Term) -> bool {
	subset
		.iter()
		.all(|(atom, polarity)| term.get(atom) == Some(polarity))
}

#[derive(Clone)]
struct GuardAtom {
	scopes: Vec<String>,
	statement: AstStatement,
}

#[derive(Default)]
struct GuardAtoms {
	atoms: BTreeMap<String, GuardAtom>,
}

impl GuardAtoms {
	fn formula_for_items(
		&mut self,
		items: &[AstStatement],
		scopes: &[String],
	) -> Result<Formula, AstAdapterError> {
		let mut result = Formula::true_value();
		for statement in items
			.iter()
			.filter(|statement| !matches!(statement, AstStatement::Comment { .. }))
		{
			result = result.and(&self.formula_for_statement(statement, scopes)?)?;
		}
		Ok(result)
	}

	fn formula_for_statement(
		&mut self,
		statement: &AstStatement,
		scopes: &[String],
	) -> Result<Formula, AstAdapterError> {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			return Ok(self.atom_formula(scopes, statement));
		};
		match key.as_str() {
			"AND" => self.formula_for_items(items, scopes),
			"OR" => {
				let mut result = Formula::false_value();
				for item in items
					.iter()
					.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				{
					result = result.or(&self.formula_for_statement(item, scopes)?)?;
				}
				Ok(result)
			}
			"NOT" => self.formula_for_items(items, scopes)?.not(),
			"NAND" => self.formula_for_items(items, scopes)?.not(),
			"NOR" => {
				let mut inner = Formula::false_value();
				for item in items
					.iter()
					.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				{
					inner = inner.or(&self.formula_for_statement(item, scopes)?)?;
				}
				inner.not()
			}
			_ if items
				.iter()
				.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				.count() == 1 =>
			{
				let mut nested_scopes = scopes.to_vec();
				nested_scopes.push(key.clone());
				let child = items
					.iter()
					.find(|item| !matches!(item, AstStatement::Comment { .. }))
					.expect("singleton scope has one semantic child");
				self.formula_for_statement(child, &nested_scopes)
			}
			_ => Ok(self.atom_formula(scopes, statement)),
		}
	}

	fn atom_formula(&mut self, scopes: &[String], statement: &AstStatement) -> Formula {
		let key = format!("{}{}", scopes.join("/"), statement_key(statement));
		self.atoms.entry(key.clone()).or_insert_with(|| GuardAtom {
			scopes: scopes.to_vec(),
			statement: statement.clone(),
		});
		Formula::literal(key)
	}

	fn formula_items(&self, formula: &Formula) -> Result<Vec<AstStatement>, AstAdapterError> {
		let disjuncts = if formula.is_false() {
			vec![bool_assignment("always", false)]
		} else {
			formula
				.terms
				.iter()
				.map(|term| {
					let literals = term
						.iter()
						.map(|(atom, polarity)| self.literal_statement(atom, *polarity))
						.collect::<Result<Vec<_>, _>>()?;
					Ok(if literals.is_empty() {
						bool_assignment("always", true)
					} else if literals.len() == 1 {
						literals.into_iter().next().expect("one literal")
					} else {
						block_assignment("AND", literals)
					})
				})
				.collect::<Result<Vec<_>, AstAdapterError>>()?
		};
		Ok(vec![block_assignment("OR", disjuncts)])
	}

	fn literal_statement(
		&self,
		atom: &str,
		polarity: bool,
	) -> Result<AstStatement, AstAdapterError> {
		let atom = self.atoms.get(atom).ok_or_else(|| {
			AstAdapterError::UnprovableControlFlow("normalized guard atom was lost".to_string())
		})?;
		let mut statement = atom.statement.clone();
		if !polarity {
			statement = block_assignment("NOT", vec![statement]);
		}
		for scope in atom.scopes.iter().rev() {
			statement = block_assignment(scope, vec![statement]);
		}
		Ok(statement)
	}
}

#[derive(Clone)]
struct RawCase {
	guard: Option<Formula>,
	effect_items: Vec<AstStatement>,
	leading_comments: Vec<AstStatement>,
	original_index: usize,
}

struct Case {
	effect_key: String,
	effect_items: Vec<AstStatement>,
	effective_guard: Formula,
	leading_comments: Vec<AstStatement>,
	original_rank: usize,
}

pub(super) fn starts_chain(statement: &AstStatement) -> bool {
	assignment_key(statement) == Some("if")
}

pub(super) fn normalize_chain(
	statements: &[AstStatement],
	start: usize,
	policy: &impl ClausewitzTreePolicy,
) -> Result<(TreeNode, usize), AstAdapterError> {
	let mut atoms = GuardAtoms::default();
	let (raw_cases, next, complete) = extract_raw_cases(statements, start, &mut atoms)?;
	let mut seen_guards = BTreeSet::new();
	for case in &raw_cases {
		if let Some(guard) = &case.guard
			&& !seen_guards.insert(guard.key())
		{
			return Err(AstAdapterError::DuplicateControlFlowGuard(guard.key()));
		}
	}

	let mut coverage = Formula::false_value();
	let mut cases_by_effect: BTreeMap<String, Case> = BTreeMap::new();
	for raw in raw_cases {
		let effective_guard = match &raw.guard {
			Some(guard) => guard.and(&coverage.not()?)?,
			None => coverage.not()?,
		};
		if effective_guard.is_false() {
			return Err(AstAdapterError::UnprovableControlFlow(format!(
				"branch {} is unreachable",
				raw.original_index
			)));
		}
		if let Some(guard) = &raw.guard {
			coverage = coverage.or(guard)?;
		}
		let effect_key = normalized_effect_key(&raw.effect_items, policy)?;
		match cases_by_effect.get_mut(&effect_key) {
			Some(existing) => {
				existing.effective_guard = existing.effective_guard.or(&effective_guard)?;
				existing.leading_comments.extend(raw.leading_comments);
				existing.original_rank = existing.original_rank.min(raw.original_index);
			}
			None => {
				cases_by_effect.insert(
					effect_key.clone(),
					Case {
						effect_key,
						effect_items: raw.effect_items,
						effective_guard,
						leading_comments: raw.leading_comments,
						original_rank: raw.original_index,
					},
				);
			}
		}
	}

	let mut cases = cases_by_effect.into_values().collect::<Vec<_>>();
	if complete && cases.len() < 2 {
		return Err(AstAdapterError::UnprovableControlFlow(
			"complete chain has no distinct guarded case".to_string(),
		));
	}
	let default = if complete {
		let candidates = cases
			.iter()
			.enumerate()
			.filter(|(_, case)| case.effective_guard.is_negative_only())
			.map(|(index, _)| index)
			.collect::<Vec<_>>();
		match candidates.as_slice() {
			[index] => Some(*index),
			_ => {
				return Err(AstAdapterError::UnprovableControlFlow(format!(
					"complete chain has {} possible default cases",
					candidates.len()
				)));
			}
		}
	} else {
		None
	};
	let order = stable_case_order(&cases, default)?;
	let default_effect = default.map(|index| cases[index].effect_key.clone());
	let mut children = Vec::new();
	let mut prior_positive_atoms = BTreeSet::new();
	let mut ordered_coverage = Formula::false_value();
	for index in order {
		let case = &mut cases[index];
		for comment in &case.leading_comments {
			children.push(normalize_statement(comment, policy)?);
		}
		let is_default = Some(index) == default;
		let value = if is_default {
			AstValue::Block {
				items: case.effect_items.clone(),
				span: synthetic_span(),
			}
		} else {
			let mut selector = case
				.effective_guard
				.without_prior_negations(&prior_positive_atoms)?;
			let actual = selector.and(&ordered_coverage.not()?)?;
			if actual != case.effective_guard {
				selector = case.effective_guard.clone();
			}
			let mut items = vec![block_assignment("limit", atoms.formula_items(&selector)?)];
			items.extend(case.effect_items.clone());
			AstValue::Block {
				items,
				span: synthetic_span(),
			}
		};
		let kind = if is_default {
			ELSE_BRANCH_KIND.to_string()
		} else {
			guarded_branch_kind(&value)
		};
		let mut node = branch(
			&kind,
			None,
			Some(SemanticKey::parent_scoped(
				"clausewitz.control_flow.branch.effect",
				case.effect_key.clone(),
			)),
			ChildOrder::Ordered,
			ChildCardinality::ExactlyOne,
			vec![normalize_value(
				&value,
				Some(if is_default { "else" } else { "if" }),
				policy,
			)?],
		);
		node.signature = Some(format!("effect:{}", case.effect_key));
		children.push(node);
		prior_positive_atoms.extend(case.effective_guard.positive_atoms());
		ordered_coverage = ordered_coverage.or(&case.effective_guard)?;
	}

	let identity = default_effect.or_else(|| {
		children
			.iter()
			.find_map(|child| child.signature.as_ref().cloned())
	});
	let exclusive = children
		.iter()
		.any(|child| child.kind.contains(":exclusive:"));
	let chain_kind = format!(
		"{CHAIN_KIND_PREFIX}{}{}",
		if exclusive { "exclusive:" } else { "" },
		if complete { "complete" } else { "open" }
	);
	let mut chain = branch(
		&chain_kind,
		None,
		identity.as_ref().map(|identity| {
			SemanticKey::parent_scoped("clausewitz.control_flow.chain.effect", identity.clone())
		}),
		ChildOrder::Ordered,
		ChildCardinality::Many,
		children,
	);
	chain.signature = identity;
	Ok((chain, next))
}

fn extract_raw_cases(
	statements: &[AstStatement],
	start: usize,
	atoms: &mut GuardAtoms,
) -> Result<(Vec<RawCase>, usize, bool), AstAdapterError> {
	let mut cases = Vec::new();
	let mut cursor = start;
	let mut leading_comments = Vec::new();
	let mut complete = false;
	loop {
		let statement = statements.get(cursor).ok_or_else(|| {
			AstAdapterError::UnprovableControlFlow("control-flow chain ended early".to_string())
		})?;
		let key = assignment_key(statement).ok_or_else(|| {
			AstAdapterError::UnprovableControlFlow("branch is not an assignment".to_string())
		})?;
		if !matches!(key, "if" | "else_if" | "else") {
			return Err(AstAdapterError::UnprovableControlFlow(format!(
				"unexpected `{key}` in control-flow chain"
			)));
		}
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			return Err(AstAdapterError::UnprovableControlFlow(format!(
				"`{key}` branch is not a block"
			)));
		};
		let (guard, effect_items) = if key == "else" {
			complete = true;
			(None, items.clone())
		} else {
			let limits = items
				.iter()
				.enumerate()
				.filter(|(_, item)| assignment_key(item) == Some("limit"))
				.collect::<Vec<_>>();
			let [
				(
					limit_index,
					AstStatement::Assignment {
						value: AstValue::Block {
							items: limit_items, ..
						},
						..
					},
				),
			] = limits.as_slice()
			else {
				return Err(AstAdapterError::UnprovableControlFlow(format!(
					"`{key}` branch requires exactly one block `limit`"
				)));
			};
			(
				Some(atoms.formula_for_items(limit_items, &[])?),
				items
					.iter()
					.enumerate()
					.filter(|(index, _)| *index != *limit_index)
					.map(|(_, item)| item.clone())
					.collect(),
			)
		};
		cases.push(RawCase {
			guard,
			effect_items,
			leading_comments: std::mem::take(&mut leading_comments),
			original_index: cases.len(),
		});
		cursor += 1;
		if complete {
			break;
		}

		let mut next = cursor;
		while statements
			.get(next)
			.is_some_and(|statement| matches!(statement, AstStatement::Comment { .. }))
		{
			next += 1;
		}
		let Some("else_if" | "else") = statements.get(next).and_then(assignment_key) else {
			break;
		};
		leading_comments.extend_from_slice(&statements[cursor..next]);
		cursor = next;
	}
	Ok((cases, cursor, complete))
}

fn normalized_effect_key(
	items: &[AstStatement],
	policy: &impl ClausewitzTreePolicy,
) -> Result<String, AstAdapterError> {
	let value = AstValue::Block {
		items: items.to_vec(),
		span: synthetic_span(),
	};
	let tree = NormalizedTree::from_root(normalize_value(
		&value,
		Some("control_flow_effect"),
		policy,
	)?)?;
	Ok(tree.node(tree.root())?.subtree_hash.to_string())
}

fn stable_case_order(
	cases: &[Case],
	default: Option<usize>,
) -> Result<Vec<usize>, AstAdapterError> {
	let mut positive_owners: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
	for (index, case) in cases.iter().enumerate() {
		for atom in case.effective_guard.positive_atoms() {
			positive_owners.entry(atom).or_default().insert(index);
		}
	}
	let mut outgoing = vec![BTreeSet::new(); cases.len()];
	let mut indegree = vec![0usize; cases.len()];
	for (target, case) in cases.iter().enumerate() {
		for atom in case.effective_guard.common_negative_atoms() {
			let Some(owners) = positive_owners.get(&atom) else {
				continue;
			};
			if owners.len() != 1 {
				return Err(AstAdapterError::UnprovableControlFlow(format!(
					"guard atom `{atom}` has {} precedence owners",
					owners.len()
				)));
			}
			let source = *owners.first().expect("one owner");
			if source != target && outgoing[source].insert(target) {
				indegree[target] += 1;
			}
		}
	}
	if let Some(default) = default {
		for (source, successors) in outgoing.iter_mut().enumerate() {
			if source != default && successors.insert(default) {
				indegree[default] += 1;
			}
		}
	}

	let mut ready = indegree
		.iter()
		.enumerate()
		.filter(|(_, count)| **count == 0)
		.map(|(index, _)| {
			(
				cases[index].original_rank,
				cases[index].effect_key.clone(),
				index,
			)
		})
		.collect::<BTreeSet<_>>();
	let mut ordered = Vec::with_capacity(cases.len());
	while let Some((_, _, index)) = ready.pop_first() {
		ordered.push(index);
		for successor in &outgoing[index] {
			indegree[*successor] -= 1;
			if indegree[*successor] == 0 {
				ready.insert((
					cases[*successor].original_rank,
					cases[*successor].effect_key.clone(),
					*successor,
				));
			}
		}
	}
	if ordered.len() != cases.len() {
		return Err(AstAdapterError::UnprovableControlFlow(
			"source precedence constraints contain a cycle".to_string(),
		));
	}
	Ok(ordered)
}

fn guarded_branch_kind(value: &AstValue) -> String {
	let AstValue::Block { items, .. } = value else {
		return format!("{GUARDED_BRANCH_KIND_PREFIX}scalar");
	};
	let mut effects = items
		.iter()
		.filter_map(assignment_key)
		.filter(|key| *key != "limit")
		.collect::<Vec<_>>();
	effects.sort_unstable();
	effects.dedup();
	let role = if contains_assignment_key(value, "dynasty") {
		"exclusive:"
	} else {
		""
	};
	format!(
		"{GUARDED_BRANCH_KIND_PREFIX}{role}{}",
		if effects.is_empty() {
			"empty".to_string()
		} else {
			effects.join("+")
		}
	)
}

fn contains_assignment_key(value: &AstValue, expected: &str) -> bool {
	let AstValue::Block { items, .. } = value else {
		return false;
	};
	items.iter().any(|statement| match statement {
		AstStatement::Assignment { key, value, .. } => {
			key == expected || contains_assignment_key(value, expected)
		}
		AstStatement::Item { value, .. } => contains_assignment_key(value, expected),
		AstStatement::Comment { .. } => false,
	})
}

pub(super) fn is_chain_kind(kind: &str) -> bool {
	kind.starts_with(CHAIN_KIND_PREFIX)
}

fn is_guarded_branch_kind(kind: &str) -> bool {
	kind.starts_with(GUARDED_BRANCH_KIND_PREFIX)
}

pub(super) fn denormalize_chain(
	tree: &NormalizedTree,
	chain_id: NodeId,
	chain: &NormalizedNode,
) -> Result<Vec<AstStatement>, AstAdapterError> {
	let mut statements = Vec::with_capacity(chain.children.len());
	let mut branch_count = 0;
	let mut saw_else = false;
	let mut branch_keys = Vec::new();
	for (position, child) in chain.children.iter().enumerate() {
		let node = tree.node(*child)?;
		if node.kind == COMMENT_KIND {
			statements.push(denormalize_statement(tree, *child)?);
			continue;
		}
		if is_guarded_branch_kind(&node.kind) {
			if saw_else {
				return Err(AstAdapterError::InvalidTree(format!(
					"guarded branch at child {position} follows `else` in control-flow chain {}",
					chain_id.get()
				)));
			}
			let key = if branch_count == 0 { "if" } else { "else_if" };
			branch_count += 1;
			branch_keys.push(key.to_string());
			let mut value = denormalize_only_value_child(tree, node)?;
			simplify_guard(&mut value)?;
			statements.push(AstStatement::Assignment {
				key: key.to_string(),
				key_span: synthetic_span(),
				value,
				span: synthetic_span(),
			});
			continue;
		}
		if node.kind == ELSE_BRANCH_KIND {
			if branch_count == 0 || saw_else {
				return Err(AstAdapterError::InvalidTree(format!(
					"invalid `else` placement at child {position} in control-flow chain {} after [{}]",
					chain_id.get(),
					branch_keys.join(", ")
				)));
			}
			saw_else = true;
			branch_keys.push("else".to_string());
			let value = denormalize_only_value_child(tree, node)?;
			if value_contains_limit(&value) {
				return Err(AstAdapterError::InvalidTree(
					"control-flow `else` contains a synthetic guard".to_string(),
				));
			}
			statements.push(AstStatement::Assignment {
				key: "else".to_string(),
				key_span: synthetic_span(),
				value,
				span: synthetic_span(),
			});
			continue;
		}
		return Err(AstAdapterError::InvalidTree(format!(
			"control-flow chain {} contains non-branch child {position}",
			chain_id.get()
		)));
	}
	if branch_count == 0 {
		return Err(AstAdapterError::InvalidTree(
			"control-flow chain contains no guarded branches".to_string(),
		));
	}
	Ok(statements)
}

fn simplify_guard(value: &mut AstValue) -> Result<(), AstAdapterError> {
	let AstValue::Block { items, .. } = value else {
		return Err(AstAdapterError::InvalidTree(
			"guarded control-flow branch is not a block".to_string(),
		));
	};
	let mut limits = items
		.iter_mut()
		.filter(|item| assignment_key(item) == Some("limit"));
	let Some(AstStatement::Assignment {
		value: AstValue::Block {
			items: limit_items, ..
		},
		..
	}) = limits.next()
	else {
		return Err(AstAdapterError::InvalidTree(
			"guarded control-flow branch requires one block `limit`".to_string(),
		));
	};
	if limits.next().is_some() {
		return Err(AstAdapterError::InvalidTree(
			"guarded control-flow branch requires one block `limit`".to_string(),
		));
	}
	let [
		AstStatement::Assignment {
			key,
			value: AstValue::Block {
				items: disjuncts, ..
			},
			..
		},
	] = limit_items.as_slice()
	else {
		return Ok(());
	};
	if key != "OR" || disjuncts.len() != 1 {
		return Ok(());
	}
	let only = disjuncts[0].clone();
	*limit_items = match only {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == "AND" => items,
		other => vec![other],
	};
	Ok(())
}

fn value_contains_limit(value: &AstValue) -> bool {
	matches!(value, AstValue::Block { items, .. } if items.iter().any(|item| assignment_key(item) == Some("limit")))
}

fn block_assignment(key: &str, items: Vec<AstStatement>) -> AstStatement {
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

fn bool_assignment(key: &str, value: bool) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Scalar {
			value: ScalarValue::Bool(value),
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn statement_key(statement: &AstStatement) -> String {
	match statement {
		AstStatement::Assignment { key, value, .. } => format!("/{key}={}", value_key(value)),
		AstStatement::Item { value, .. } => format!("/item={}", value_key(value)),
		AstStatement::Comment { text, .. } => format!("/comment={text}"),
	}
}

fn value_key(value: &AstValue) -> String {
	match value {
		AstValue::Scalar { value, .. } => format!("scalar:{}", value.as_text()),
		AstValue::Block { items, .. } => format!(
			"block:[{}]",
			items
				.iter()
				.map(statement_key)
				.collect::<Vec<_>>()
				.join(",")
		),
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::MergePolicies;
	use foch_language::analyzer::parser::{
		AstFile, AstStatement, AstValue, parse_clausewitz_content,
	};
	use foch_merge_kernel::ConflictKind;

	use crate::emit::emit_clausewitz_statements;

	use super::super::ast_adapter::{denormalize_ast, normalize_ast};
	use super::super::policy::DefaultClausewitzTreePolicy;
	use super::super::{AstAdapterError, merge_clausewitz_files};

	fn parse(source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn emit(file: &AstFile) -> String {
		emit_clausewitz_statements(&file.statements).expect("emit Clausewitz AST")
	}

	fn block_items<'a>(statements: &'a [AstStatement], key: &str) -> &'a [AstStatement] {
		statements
			.iter()
			.find_map(|statement| match statement {
				AstStatement::Assignment {
					key: candidate,
					value: AstValue::Block { items, .. },
					..
				} if candidate == key => Some(items.as_slice()),
				_ => None,
			})
			.unwrap_or_else(|| panic!("missing `{key}` block"))
	}

	fn scalar_for_key(statements: &[AstStatement], expected: &str) -> Option<String> {
		statements.iter().find_map(|statement| match statement {
			AstStatement::Assignment { key, value, .. } if key == expected => match value {
				AstValue::Scalar { value, .. } => Some(value.as_text()),
				AstValue::Block { items, .. } => scalar_for_key(items, expected),
			},
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			}
			| AstStatement::Item {
				value: AstValue::Block { items, .. },
				..
			} => scalar_for_key(items, expected),
			AstStatement::Assignment { .. }
			| AstStatement::Item { .. }
			| AstStatement::Comment { .. } => None,
		})
	}

	fn branch_shape(file: &AstFile) -> Vec<(String, Option<String>, String)> {
		let trigger = block_items(block_items(&file.statements, "coal"), "trigger");
		trigger
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
				if !matches!(key.as_str(), "if" | "else_if" | "else") {
					return None;
				}
				let guard = items.iter().find_map(|item| match item {
					AstStatement::Assignment {
						key,
						value: AstValue::Block { items, .. },
						..
					} if key == "limit" => scalar_for_key(items, "has_province_flag")
						.or_else(|| scalar_for_key(items, "has_country_flag")),
					_ => None,
				});
				let effect = scalar_for_key(items, "adm_tech").unwrap_or_else(|| {
					assert_eq!(
						scalar_for_key(items, "has_institution").as_deref(),
						Some("enlightenment")
					);
					"default".to_string()
				});
				Some((key.clone(), guard, effect))
			})
			.collect()
	}

	fn branch(flag: &str, effect: &str) -> String {
		format!("limit = {{ has_country_flag = {flag} }} add_prestige = {effect}")
	}

	#[test]
	fn composes_coal_cases_by_effect_and_source_precedence() {
		let base = parse(
			"coal = {\n\
			\ttrigger = {\n\
			\t\tif = {\n\
			\t\t\tlimit = { owner = { has_country_flag = earlier_coal_available } }\n\
			\t\t\towner = { has_institution = manufactories adm_tech = 23 }\n\
			\t\t}\n\
			\t\telse = { owner = { has_institution = enlightenment } }\n\
			\t}\n\
			}\n",
		);
		let left = parse(
			"coal = {\n\
			\ttrigger = {\n\
			\t\tNOT = { has_province_flag = has_latent_good }\n\
			\t\tif = {\n\
			\t\t\tlimit = {\n\
			\t\t\t\tNOT = { has_province_flag = GER_specific_coal }\n\
			\t\t\t\towner = { NOT = { has_country_flag = earlier_coal_available } }\n\
			\t\t\t}\n\
			\t\t\towner = { has_institution = enlightenment }\n\
			\t\t}\n\
			\t\telse_if = {\n\
			\t\t\tlimit = { has_province_flag = GER_specific_coal }\n\
			\t\t\towner = { has_institution = manufactories adm_tech = 21 }\n\
			\t\t}\n\
			\t\telse = { owner = { has_institution = manufactories adm_tech = 23 } }\n\
			\t}\n\
			}\n",
		);
		let right = parse(
			"coal = {\n\
			\ttrigger = {\n\
			\t\tif = {\n\
			\t\t\tlimit = { owner = { has_country_flag = earlier_coal_available } }\n\
			\t\t\towner = { has_institution = manufactories adm_tech = 23 }\n\
			\t\t}\n\
			\t\telse_if = {\n\
			\t\t\tlimit = { owner = { has_country_flag = ENG_early_inno_coal } }\n\
			\t\t\towner = { has_institution = manufactories innovativeness = 80 adm_tech = 22 }\n\
			\t\t}\n\
			\t\telse = { owner = { has_institution = enlightenment } }\n\
			\t}\n\
			}\n",
		);

		let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
			.expect("merge coal control-flow chains");

		assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
		let resolved = outcome.resolved_ast().expect("publishable coal AST");
		assert_eq!(
			branch_shape(resolved),
			vec![
				(
					"if".to_string(),
					Some("GER_specific_coal".to_string()),
					"21".to_string()
				),
				(
					"else_if".to_string(),
					Some("earlier_coal_available".to_string()),
					"23".to_string(),
				),
				(
					"else_if".to_string(),
					Some("ENG_early_inno_coal".to_string()),
					"22".to_string(),
				),
				("else".to_string(), None, "default".to_string()),
			]
		);
		assert!(emit(resolved).contains("has_province_flag = has_latent_good"));
	}

	#[test]
	fn keeps_adjacent_independent_chains_separate() {
		let ast = parse(
			"coal = { trigger = {\n\
			\tif = { limit = { has_country_flag = first } add_prestige = 1 }\n\
			\tif = { limit = { has_country_flag = second } add_stability = 1 }\n\
			} }\n",
		);

		let tree = normalize_ast(&ast, &DefaultClausewitzTreePolicy).expect("normalize AST");
		let chain_count = tree
			.nodes()
			.filter(|(_, node)| node.kind.starts_with("clausewitz.control_flow.chain:"))
			.count();
		let rebuilt = denormalize_ast(ast.path.clone(), &tree).expect("rebuild AST");

		assert_eq!(chain_count, 2);
		assert_eq!(emit(&rebuilt), emit(&ast));
	}

	#[test]
	fn accepts_a_one_sided_leading_case_insertion() {
		let base = parse(&format!(
			"coal = {{ trigger = {{ if = {{ {} }} else = {{ add_prestige = 0 }} }} }}\n",
			branch("base", "1")
		));
		let left = parse(&format!(
			"coal = {{ trigger = {{ if = {{ {} }} else_if = {{ {} }} else = {{ add_prestige = 0 }} }} }}\n",
			branch("inserted", "2"),
			branch("base", "1")
		));

		let outcome = merge_clausewitz_files(&base, &left, &base, &MergePolicies::default())
			.expect("merge one-sided branch insertion");

		assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
		assert_eq!(emit(outcome.resolved_ast().unwrap()), emit(&left));
	}

	#[test]
	fn coalesces_same_effect_cases_and_unions_their_guards() {
		let base = parse(&format!(
			"coal = {{ trigger = {{ if = {{ {} }} else = {{ add_stability = 0 }} }} }}\n",
			branch("base", "1")
		));
		let left = parse(&format!(
			"coal = {{ trigger = {{ if = {{ {} }} else_if = {{ {} }} else = {{ add_stability = 0 }} }} }}\n",
			branch("base", "1"),
			branch("left", "1")
		));
		let right = parse(&format!(
			"coal = {{ trigger = {{ if = {{ {} }} else_if = {{ {} }} else = {{ add_stability = 0 }} }} }}\n",
			branch("base", "1"),
			branch("right", "1")
		));

		let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
			.expect("merge guards for equal effects");

		assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
		let output = emit(outcome.resolved_ast().unwrap());
		assert_eq!(output.matches("add_prestige = 1").count(), 1, "{output}");
		for flag in ["base", "left", "right"] {
			assert!(
				output.contains(&format!("has_country_flag = {flag}")),
				"{output}"
			);
		}
	}

	#[test]
	fn rejects_duplicate_guards_before_publication() {
		let source = parse(
			"coal = { trigger = {\n\
			\tif = { limit = { has_country_flag = repeated } add_prestige = 1 }\n\
			\telse_if = { limit = { has_country_flag = repeated } add_stability = 1 }\n\
			\telse = { add_legitimacy = 1 }\n\
			} }\n",
		);

		let error = merge_clausewitz_files(&source, &source, &source, &MergePolicies::default())
			.expect_err("duplicate guards must be rejected");

		assert!(
			matches!(error, AstAdapterError::DuplicateControlFlowGuard(_)),
			"{error:?}"
		);
	}

	#[test]
	fn rejects_guardless_branches_as_unprovable() {
		let source = parse(
			"coal = { trigger = {\n\
			\tif = { add_prestige = 1 }\n\
			\telse = { add_stability = 1 }\n\
			} }\n",
		);

		let error = merge_clausewitz_files(&source, &source, &source, &MergePolicies::default())
			.expect_err("guardless branches must be rejected");

		assert!(
			matches!(error, AstAdapterError::UnprovableControlFlow(_)),
			"{error:?}"
		);
	}

	#[test]
	fn withholds_precedence_cycles_as_typed_ordering_conflicts() {
		let source = |order: [&str; 3]| {
			parse(&format!(
				"coal = {{ trigger = {{\n\
				\tif = {{ {} }}\n\
				\telse_if = {{ {} }}\n\
				\telse_if = {{ {} }}\n\
				\telse = {{ add_legitimacy = 0 }}\n\
				}} }}\n",
				branch(order[0], order[0]),
				branch(order[1], order[1]),
				branch(order[2], order[2]),
			))
		};
		let base = source(["1", "2", "3"]);
		let left = source(["2", "1", "3"]);
		let right = source(["1", "3", "2"]);

		let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
			.expect("ordering conflict remains a valid tentative merge");

		assert!(outcome.resolved_ast().is_none());
		assert!(
			outcome
				.conflicts()
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::Ordering),
			"{:?}",
			outcome.conflicts()
		);
	}

	#[test]
	fn keeps_nested_chain_guards_scoped_to_their_outer_case() {
		let ast = parse(
			"coal = { trigger = {\n\
			\tif = {\n\
			\t\tlimit = { has_country_flag = outer }\n\
			\t\tif = { limit = { has_country_flag = inner } add_prestige = 1 }\n\
			\t\telse = { add_prestige = 0 }\n\
			\t}\n\
			\telse = { add_stability = 0 }\n\
			} }\n",
		);

		let tree = normalize_ast(&ast, &DefaultClausewitzTreePolicy).expect("normalize nested AST");
		let rebuilt = denormalize_ast(ast.path.clone(), &tree).expect("rebuild nested AST");

		assert_eq!(emit(&rebuilt), emit(&ast));
	}
}
