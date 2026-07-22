use std::collections::{BTreeMap, BTreeSet};

use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, ScalarValue, SpanRange};

/// Trivia detached from a Clausewitz AST before structural matching.
///
/// The entries are intentionally opaque to the GumTree/PCS adapter. Their
/// attachment addresses contain semantic parents and siblings, but comment
/// text never contributes to a semantic node identity or conflict.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Trivia {
	entries: Vec<TriviaEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TriviaEntry {
	attachment: Attachment,
	text: String,
	span: SpanRange,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct SemanticParent(Vec<SemanticSibling>);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct SemanticSibling {
	kind: SemanticSiblingKind,
	occurrence: usize,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum SemanticSiblingKind {
	Assignment(String),
	Item([u8; 32]),
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Attachment {
	parent: SemanticParent,
	position: AttachmentPosition,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum AttachmentPosition {
	Before(SemanticSibling),
	After(SemanticSibling),
	ParentEnd,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct TriviaIdentity {
	attachment: Attachment,
	text: String,
}

/// Remove comments recursively and return the semantic AST plus its trivia
/// sidecar. Spans are preserved in both outputs but never participate in
/// semantic attachment identities.
pub(crate) fn detach_trivia(file: &AstFile) -> (AstFile, Trivia) {
	let mut trivia = Trivia::default();
	let statements = detach_statements(
		&file.statements,
		&SemanticParent(Vec::new()),
		&mut trivia.entries,
	);
	(
		AstFile {
			path: file.path.clone(),
			statements,
		},
		trivia,
	)
}

/// Merge comment multisets from the two active revisions.
///
/// Base entries that remain active are emitted first, followed by new left and
/// right entries. Equal comments at the same attachment point deduplicate
/// across revisions while repeated equal comments within one revision retain
/// their maximum active multiplicity. A base comment deleted by both active
/// revisions is therefore absent from the result.
pub(crate) fn merge_trivia(base: &Trivia, left: &Trivia, right: &Trivia) -> Trivia {
	let left_counts = trivia_counts(left);
	let right_counts = trivia_counts(right);
	let mut active_counts = left_counts;
	for (identity, count) in right_counts {
		active_counts
			.entry(identity)
			.and_modify(|active| *active = (*active).max(count))
			.or_insert(count);
	}

	let mut emitted_counts = BTreeMap::<TriviaIdentity, usize>::new();
	let mut entries = Vec::new();
	for source in [base, left, right] {
		for entry in &source.entries {
			let identity = trivia_identity(entry);
			let target = active_counts.get(&identity).copied().unwrap_or_default();
			let emitted = emitted_counts.entry(identity).or_default();
			if *emitted < target {
				entries.push(entry.clone());
				*emitted += 1;
			}
		}
	}
	Trivia { entries }
}

/// Attach merged trivia to a semantic AST.
///
/// An attachment is restored next to its semantic sibling. If that sibling is
/// absent from the merged parent, the comment is appended to the parent. A
/// comment whose semantic parent no longer exists is omitted with that parent.
pub(crate) fn attach_trivia(file: &mut AstFile, trivia: &Trivia) {
	remove_comments(&mut file.statements);
	let mut by_parent = BTreeMap::<SemanticParent, Vec<&TriviaEntry>>::new();
	for entry in &trivia.entries {
		by_parent
			.entry(entry.attachment.parent.clone())
			.or_default()
			.push(entry);
	}
	attach_statements(
		&mut file.statements,
		&SemanticParent(Vec::new()),
		&mut by_parent,
	);
}

fn detach_statements(
	statements: &[AstStatement],
	parent: &SemanticParent,
	entries: &mut Vec<TriviaEntry>,
) -> Vec<AstStatement> {
	let siblings = semantic_siblings(statements);
	let (previous, next) = nearest_semantic_siblings(&siblings);
	let mut semantic = Vec::with_capacity(statements.len());

	for (index, statement) in statements.iter().enumerate() {
		if let AstStatement::Comment { text, span } = statement {
			entries.push(TriviaEntry {
				attachment: Attachment {
					parent: parent.clone(),
					position: nearest_attachment(index, &previous, &next),
				},
				text: text.clone(),
				span: span.clone(),
			});
			continue;
		}

		let sibling = siblings[index]
			.clone()
			.expect("semantic statements have sibling identities");
		let child_parent = semantic_child_parent(parent, sibling);
		semantic.push(detach_statement(statement, &child_parent, entries));
	}
	semantic
}

fn detach_statement(
	statement: &AstStatement,
	child_parent: &SemanticParent,
	entries: &mut Vec<TriviaEntry>,
) -> AstStatement {
	let mut statement = statement.clone();
	match &mut statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			detach_value(value, child_parent, entries);
		}
		AstStatement::Comment { .. } => unreachable!("comments are handled by detach_statements"),
	}
	statement
}

fn detach_value(value: &mut AstValue, parent: &SemanticParent, entries: &mut Vec<TriviaEntry>) {
	if let AstValue::Block { items, .. } = value {
		*items = detach_statements(items, parent, entries);
	}
}

fn semantic_child_parent(parent: &SemanticParent, sibling: SemanticSibling) -> SemanticParent {
	let mut path = parent.0.clone();
	path.push(sibling);
	SemanticParent(path)
}

fn semantic_siblings(statements: &[AstStatement]) -> Vec<Option<SemanticSibling>> {
	let mut occurrences = BTreeMap::<SemanticSiblingKind, usize>::new();
	statements
		.iter()
		.map(|statement| {
			let kind = semantic_sibling_kind(statement)?;
			let occurrence = occurrences.entry(kind.clone()).or_default();
			let sibling = SemanticSibling {
				kind,
				occurrence: *occurrence,
			};
			*occurrence += 1;
			Some(sibling)
		})
		.collect()
}

fn semantic_sibling_kind(statement: &AstStatement) -> Option<SemanticSiblingKind> {
	match statement {
		AstStatement::Assignment { key, .. } => Some(SemanticSiblingKind::Assignment(key.clone())),
		AstStatement::Item { value, .. } => {
			Some(SemanticSiblingKind::Item(semantic_value_digest(value)))
		}
		AstStatement::Comment { .. } => None,
	}
}

type NearestSibling = Option<(usize, SemanticSibling)>;

fn nearest_semantic_siblings(
	siblings: &[Option<SemanticSibling>],
) -> (Vec<NearestSibling>, Vec<NearestSibling>) {
	let mut previous = Vec::with_capacity(siblings.len());
	let mut closest = None;
	for (index, sibling) in siblings.iter().enumerate() {
		previous.push(closest.clone());
		if let Some(sibling) = sibling {
			closest = Some((index, sibling.clone()));
		}
	}

	let mut next = vec![None; siblings.len()];
	closest = None;
	for (index, sibling) in siblings.iter().enumerate().rev() {
		next[index] = closest.clone();
		if let Some(sibling) = sibling {
			closest = Some((index, sibling.clone()));
		}
	}
	(previous, next)
}

fn nearest_attachment(
	index: usize,
	previous: &[NearestSibling],
	next: &[NearestSibling],
) -> AttachmentPosition {
	match (&previous[index], &next[index]) {
		(Some((previous_index, sibling)), Some((next_index, next_sibling))) => {
			if index - previous_index < next_index - index {
				AttachmentPosition::After(sibling.clone())
			} else {
				AttachmentPosition::Before(next_sibling.clone())
			}
		}
		(Some((_, sibling)), None) => AttachmentPosition::After(sibling.clone()),
		(None, Some((_, sibling))) => AttachmentPosition::Before(sibling.clone()),
		(None, None) => AttachmentPosition::ParentEnd,
	}
}

fn semantic_value_digest(value: &AstValue) -> [u8; 32] {
	let mut hasher = blake3::Hasher::new();
	hash_value(value, &mut hasher);
	*hasher.finalize().as_bytes()
}

fn hash_value(value: &AstValue, hasher: &mut blake3::Hasher) {
	match value {
		AstValue::Scalar { value, .. } => {
			hasher.update(&[0]);
			hash_scalar(value, hasher);
		}
		AstValue::Block { items, .. } => {
			hasher.update(&[1]);
			for statement in items {
				hash_statement(statement, hasher);
			}
			hasher.update(&[2]);
		}
	}
}

fn hash_statement(statement: &AstStatement, hasher: &mut blake3::Hasher) {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			hasher.update(&[3]);
			hash_bytes(key.as_bytes(), hasher);
			hash_value(value, hasher);
		}
		AstStatement::Item { value, .. } => {
			hasher.update(&[4]);
			hash_value(value, hasher);
		}
		AstStatement::Comment { .. } => {}
	}
}

fn hash_scalar(value: &ScalarValue, hasher: &mut blake3::Hasher) {
	match value {
		ScalarValue::Identifier(value) => {
			hasher.update(&[5]);
			hash_bytes(value.as_bytes(), hasher);
		}
		ScalarValue::String(value) => {
			hasher.update(&[6]);
			hash_bytes(value.as_bytes(), hasher);
		}
		ScalarValue::Number(value) => {
			hasher.update(&[7]);
			hash_bytes(value.as_bytes(), hasher);
		}
		ScalarValue::Bool(value) => {
			hasher.update(&[8, u8::from(*value)]);
		}
	}
}

fn hash_bytes(bytes: &[u8], hasher: &mut blake3::Hasher) {
	let length = u64::try_from(bytes.len()).expect("AST text length fits in u64");
	hasher.update(&length.to_be_bytes());
	hasher.update(bytes);
}

fn trivia_counts(trivia: &Trivia) -> BTreeMap<TriviaIdentity, usize> {
	let mut counts = BTreeMap::new();
	for entry in &trivia.entries {
		*counts.entry(trivia_identity(entry)).or_default() += 1;
	}
	counts
}

fn trivia_identity(entry: &TriviaEntry) -> TriviaIdentity {
	TriviaIdentity {
		attachment: entry.attachment.clone(),
		text: entry.text.clone(),
	}
}

fn remove_comments(statements: &mut Vec<AstStatement>) {
	statements.retain_mut(|statement| match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			if let AstValue::Block { items, .. } = value {
				remove_comments(items);
			}
			true
		}
		AstStatement::Comment { .. } => false,
	});
}

fn attach_statements(
	statements: &mut Vec<AstStatement>,
	parent: &SemanticParent,
	by_parent: &mut BTreeMap<SemanticParent, Vec<&TriviaEntry>>,
) {
	let siblings = semantic_siblings(statements);
	for (statement, sibling) in statements.iter_mut().zip(&siblings) {
		let sibling = sibling
			.clone()
			.expect("attach_trivia removes comments before computing identities");
		let child_parent = semantic_child_parent(parent, sibling);
		match statement {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				if let AstValue::Block { items, .. } = value {
					attach_statements(items, &child_parent, by_parent);
				}
			}
			AstStatement::Comment { .. } => {
				unreachable!("attach_trivia removes comments before traversal")
			}
		}
	}

	let Some(entries) = by_parent.remove(parent) else {
		return;
	};
	let known_siblings = siblings.iter().flatten().cloned().collect::<BTreeSet<_>>();
	let mut before = BTreeMap::<SemanticSibling, Vec<&TriviaEntry>>::new();
	let mut after = BTreeMap::<SemanticSibling, Vec<&TriviaEntry>>::new();
	let mut parent_end = Vec::new();
	for entry in entries {
		match &entry.attachment.position {
			AttachmentPosition::Before(sibling) if known_siblings.contains(sibling) => {
				before.entry(sibling.clone()).or_default().push(entry);
			}
			AttachmentPosition::After(sibling) if known_siblings.contains(sibling) => {
				after.entry(sibling.clone()).or_default().push(entry);
			}
			AttachmentPosition::Before(_)
			| AttachmentPosition::After(_)
			| AttachmentPosition::ParentEnd => parent_end.push(entry),
		}
	}

	let semantic = std::mem::take(statements);
	let mut attached =
		Vec::with_capacity(semantic.len() + trivia_entry_count(&before, &after, &parent_end));
	for (statement, sibling) in semantic.into_iter().zip(siblings.into_iter()) {
		let sibling = sibling.expect("semantic statements have sibling identities");
		append_comments(&mut attached, before.remove(&sibling).unwrap_or_default());
		attached.push(statement);
		append_comments(&mut attached, after.remove(&sibling).unwrap_or_default());
	}
	append_comments(&mut attached, parent_end);
	*statements = attached;
}

fn trivia_entry_count(
	before: &BTreeMap<SemanticSibling, Vec<&TriviaEntry>>,
	after: &BTreeMap<SemanticSibling, Vec<&TriviaEntry>>,
	parent_end: &[&TriviaEntry],
) -> usize {
	before.values().map(Vec::len).sum::<usize>()
		+ after.values().map(Vec::len).sum::<usize>()
		+ parent_end.len()
}

fn append_comments(statements: &mut Vec<AstStatement>, entries: Vec<&TriviaEntry>) {
	statements.extend(entries.into_iter().map(|entry| AstStatement::Comment {
		text: entry.text.clone(),
		span: entry.span.clone(),
	}));
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::parser::{Span, SpanRange};

	use super::*;

	#[test]
	fn identical_comments_deduplicate_outside_semantic_ast() {
		let base = file(vec![assignment("a", number("1"))]);
		let left = file(vec![assignment("a", number("1")), comment("shared")]);
		let right = left.clone();

		let (base_semantic, base_trivia) = detach_trivia(&base);
		let (left_semantic, left_trivia) = detach_trivia(&left);
		let (right_semantic, right_trivia) = detach_trivia(&right);
		assert_eq!(base_semantic, left_semantic);
		assert_eq!(left_semantic, right_semantic);

		let mut merged = base_semantic;
		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);
		assert_eq!(statement_labels(&merged.statements), ["a", "# shared"]);
	}

	#[test]
	fn distinct_comments_union_in_base_left_right_order() {
		let base = file(vec![assignment("a", number("1")), comment("base")]);
		let left = file(vec![
			assignment("a", number("1")),
			comment("base"),
			comment("left"),
		]);
		let right = file(vec![
			assignment("a", number("1")),
			comment("base"),
			comment("right"),
		]);

		let (mut merged, base_trivia) = detach_trivia(&base);
		let (_, left_trivia) = detach_trivia(&left);
		let (_, right_trivia) = detach_trivia(&right);
		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);

		assert_eq!(
			statement_labels(&merged.statements),
			["a", "# base", "# left", "# right"]
		);
	}

	#[test]
	fn comment_deleted_by_both_sides_is_removed() {
		let base = file(vec![assignment("a", number("1")), comment("obsolete")]);
		let active = file(vec![assignment("a", number("1"))]);

		let (mut merged, base_trivia) = detach_trivia(&base);
		let (_, left_trivia) = detach_trivia(&active);
		let (_, right_trivia) = detach_trivia(&active);
		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);

		assert_eq!(statement_labels(&merged.statements), ["a"]);
	}

	#[test]
	fn edited_comment_and_retained_base_comment_are_both_active() {
		let base = file(vec![assignment("a", number("1")), comment("old")]);
		let left = file(vec![assignment("a", number("1")), comment("new")]);
		let right = base.clone();

		let (mut merged, base_trivia) = detach_trivia(&base);
		let (_, left_trivia) = detach_trivia(&left);
		let (_, right_trivia) = detach_trivia(&right);
		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);

		assert_eq!(
			statement_labels(&merged.statements),
			["a", "# old", "# new"]
		);
	}

	#[test]
	fn comment_between_control_flow_branches_stays_between_branches() {
		let revision = file(vec![
			assignment("if", block(vec![assignment("limit", bool_value(true))])),
			comment("branch boundary"),
			assignment(
				"else_if",
				block(vec![assignment("limit", bool_value(false))]),
			),
			assignment("else", block(vec![assignment("effect", number("1"))])),
		]);

		let (semantic, base_trivia) = detach_trivia(&revision);
		let (_, left_trivia) = detach_trivia(&revision);
		let (_, right_trivia) = detach_trivia(&revision);
		assert_eq!(
			statement_labels(&semantic.statements),
			["if", "else_if", "else"]
		);

		let mut merged = semantic;
		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);
		assert_eq!(
			statement_labels(&merged.statements),
			["if", "# branch boundary", "else_if", "else"]
		);
	}

	#[test]
	fn governments_style_repeated_definitions_keep_distinct_source_headers() {
		let first = assignment(
			"government",
			block(vec![assignment("id", ident("monarchy"))]),
		);
		let second = assignment(
			"government",
			block(vec![assignment("id", ident("republic"))]),
		);
		let base = file(vec![first.clone(), second.clone()]);
		let left = file(vec![first.clone(), comment("ME Reforms"), second.clone()]);
		let right = file(vec![first, comment("GE Reforms"), second]);

		let (mut merged, base_trivia) = detach_trivia(&base);
		let (left_semantic, left_trivia) = detach_trivia(&left);
		let (right_semantic, right_trivia) = detach_trivia(&right);
		assert_eq!(merged, left_semantic);
		assert_eq!(left_semantic, right_semantic);

		let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
		attach_trivia(&mut merged, &trivia);
		assert_eq!(
			statement_labels(&merged.statements),
			["government", "# ME Reforms", "# GE Reforms", "government"]
		);
	}

	#[test]
	fn missing_sibling_anchor_falls_back_to_parent_end() {
		let source = file(vec![
			assignment("a", number("1")),
			comment("attached to b"),
			assignment("b", number("2")),
		]);
		let (_, trivia) = detach_trivia(&source);
		let mut merged = file(vec![assignment("a", number("1"))]);

		attach_trivia(&mut merged, &trivia);

		assert_eq!(
			statement_labels(&merged.statements),
			["a", "# attached to b"]
		);
	}

	fn file(statements: Vec<AstStatement>) -> AstFile {
		AstFile {
			path: PathBuf::from("common/governments/test.txt"),
			statements,
		}
	}

	fn assignment(key: &str, value: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value,
			span: span(),
		}
	}

	fn comment(text: &str) -> AstStatement {
		AstStatement::Comment {
			text: text.to_string(),
			span: span(),
		}
	}

	fn block(items: Vec<AstStatement>) -> AstValue {
		AstValue::Block {
			items,
			span: span(),
		}
	}

	fn ident(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Identifier(value.to_string()),
			span: span(),
		}
	}

	fn number(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Number(value.to_string()),
			span: span(),
		}
	}

	fn bool_value(value: bool) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Bool(value),
			span: span(),
		}
	}

	fn span() -> SpanRange {
		let point = Span {
			line: 1,
			column: 1,
			offset: 0,
		};
		SpanRange {
			start: point.clone(),
			end: point,
		}
	}

	fn statement_labels(statements: &[AstStatement]) -> Vec<String> {
		statements
			.iter()
			.map(|statement| match statement {
				AstStatement::Assignment { key, .. } => key.clone(),
				AstStatement::Item { .. } => "<item>".to_string(),
				AstStatement::Comment { text, .. } => format!("# {text}"),
			})
			.collect()
	}
}
