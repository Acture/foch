//! Repair of a Clausewitz script whose parse has errors.
//!
//! The file is split at its definition heads, lines that start at column 1
//! with `key =` or `key {`, and each segment is parsed alone, so an error
//! stays in its segment instead of swallowing the rest of the file. A broken
//! segment is repaired with one token edit only when that edit is the text's
//! single trustworthy reading:
//!
//! - every one-token edit that makes the segment parse as one definition
//!   gives the same tree; or
//! - one of those trees changes the tree the text gives unedited least: it
//!   alone moves the fewest statements to another parent. The indentation
//!   only checks this choice; when it clearly favours another tree the
//!   evidence conflicts and nothing is repaired.
//!
//! The chosen edits are then applied to the whole file, which must parse
//! cleanly into exactly the statements of its segments. When any of this
//! fails nothing is repaired and the first parse stands.

use super::{
	AstStatement, AstValue, ParseDiagnostic, ParseDiagnosticCode, ParsedStatements, ParserState,
	SchemaCheck, Span, SpanRange, Token, TokenKind, lex,
};
use crate::model::{SourceRepair, SourceRepairEdit, SourceRepairEvidence};
use std::collections::HashMap;

/// Most tokens the repair of one file may parse, across every segment and
/// every edit it tries. A file that needs more is left as first parsed: the
/// search runs only for files that already failed to parse, and must not
/// make a large one cost more than a bounded multiple of its own parse.
const WORK_BUDGET: usize = 30_000_000;

/// Most readings of one segment the schema is asked about.
const SCHEMA_CONTENDERS: usize = 6;

/// What checking one definition against the schema costs, in parsed tokens
/// per token of the definition.
const SCHEMA_COST_PER_TOKEN: usize = 64;

/// Most following segments a broken one is read together with, when the
/// column-1 lines that cut them may be inside its block.
const MAX_ABSORBED_HEADS: usize = 64;

pub(super) fn recover(
	source: &str,
	parsed: ParsedStatements,
	schema: Option<&SchemaCheck<'_>>,
) -> ParsedStatements {
	repair(source, &parsed, schema).unwrap_or(parsed)
}

/// A one-token edit, by index into the token stream it applies to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edit {
	Remove(usize),
	/// A `}` before the token at this index.
	InsertClosing(usize),
	/// A `{` after the `=` at this index.
	InsertOpening(usize),
}

impl Edit {
	fn shifted(self, by: usize) -> Self {
		match self {
			Self::Remove(index) => Self::Remove(index + by),
			Self::InsertClosing(index) => Self::InsertClosing(index + by),
			Self::InsertOpening(index) => Self::InsertOpening(index + by),
		}
	}
}

struct Candidate {
	edit: Edit,
	statements: Vec<AstStatement>,
	/// Statements the edit gives another parent, or adds or drops.
	moved: usize,
	/// Lines indented other than their depth.
	violations: usize,
}

fn repair(
	source: &str,
	parsed: &ParsedStatements,
	schema: Option<&SchemaCheck<'_>>,
) -> Option<ParsedStatements> {
	let mut diagnostics = Vec::new();
	let unterminated = parsed
		.diagnostics
		.iter()
		.find(|diagnostic| diagnostic.code == ParseDiagnosticCode::UnterminatedString);
	let tokens = match unterminated {
		Some(diagnostic) => {
			let (tokens, lexer_diagnostics) =
				lex(source, false, Some(diagnostic.span.start.offset));
			if !lexer_diagnostics.is_empty() {
				return None;
			}
			diagnostics.push(repaired(
				ParseDiagnosticCode::UnterminatedString,
				"string has no closing quote; it ends at the end of its line",
				diagnostic.span.clone(),
				SourceRepair {
					edit: SourceRepairEdit::ClosedStringAtLineEnd,
					evidence: SourceRepairEvidence::OnlyReading,
				},
			));
			tokens
		}
		None => lex(source, false, None).0,
	};
	let reported: Vec<usize> = parsed
		.diagnostics
		.iter()
		.map(|diagnostic| diagnostic.span.start.offset)
		.collect();
	let layout = Layout::new(source);

	let bounds = segments(&tokens);
	let mut budget = WORK_BUDGET;
	let mut edits = Vec::new();
	let mut statements = Vec::new();
	let mut next = 0;
	while next < bounds.len() {
		let start = bounds[next].0;
		// A segment may have been cut at a line inside a block that is merely
		// written at column 1. It then reads, unedited, together with the
		// segments after it; that reading is preferred to any repair, which
		// could otherwise "fix" both halves of a sound definition.
		let reach = bounds.len().min(next + MAX_ABSORBED_HEADS + 1);
		let range = |last: usize| {
			let end = bounds[last].1;
			(&tokens[start..end], tokens[end].span.start.clone())
		};
		let mut settled = None;
		for last in next..reach {
			let (segment, following) = range(last);
			budget = budget.checked_sub(segment.len())?;
			let parsed = parse_segment(segment, None, &following);
			if parsed.diagnostics.is_empty() {
				settled = Some((last, parsed.statements, None));
				break;
			}
		}
		// Only a segment on its own is repaired. Repairing one read together
		// with the next would let an edit nest a sound definition inside a
		// broken one, a reading that moves nothing while the tree the
		// indentation describes is not among those tried.
		if settled.is_none() {
			let (segment, following) = range(next);
			if let Some((candidate, evidence)) =
				repair_segment(segment, &following, &layout, &reported, schema, &mut budget)?
			{
				let edit = candidate.edit.shifted(start);
				settled = Some((next, candidate.statements, Some((edit, evidence))));
			}
		}
		let (last, segment_statements, edit) = settled?;
		statements.extend(segment_statements);
		edits.extend(edit);
		next = last + 1;
	}

	let mut repaired_tokens = tokens.clone();
	for (edit, _) in edits.iter().rev() {
		apply(
			&mut repaired_tokens,
			*edit,
			&tokens[edit_anchor(*edit)].span.start,
		);
	}
	let whole = ParserState::new(repaired_tokens).parse_file();
	if !whole.diagnostics.is_empty() || whole.statements != statements {
		return None;
	}
	for (edit, evidence) in edits {
		diagnostics.push(edit_diagnostic(&tokens, edit, evidence));
	}
	Some(ParsedStatements {
		statements: whole.statements,
		diagnostics,
	})
}

/// The `[start, end)` token ranges of the segments, `end` exclusive; the last
/// one ends at the end-of-file token.
fn segments(tokens: &[Token]) -> Vec<(usize, usize)> {
	let eof = tokens.len() - 1;
	let mut starts = vec![0];
	for index in 1..eof {
		if is_definition_head(tokens, index) {
			starts.push(index);
		}
	}
	let mut bounds = Vec::with_capacity(starts.len());
	for (position, start) in starts.iter().enumerate() {
		let end = starts.get(position + 1).copied().unwrap_or(eof);
		bounds.push((*start, end));
	}
	bounds
}

/// A token that starts a line at column 1 and opens a definition.
fn is_definition_head(tokens: &[Token], index: usize) -> bool {
	let token = &tokens[index];
	matches!(tokens[index - 1].kind, TokenKind::Newline)
		&& token.span.start.column == 1
		&& matches!(
			token.kind,
			TokenKind::Identifier(_) | TokenKind::String(_) | TokenKind::Number(_)
		) && matches!(
		tokens.get(index + 1).map(|next| &next.kind),
		Some(TokenKind::Eq | TokenKind::LBrace)
	)
}

/// The one-token repair of a broken segment, `Ok(None)` when it has no
/// single trustworthy reading, or `None` when the file's work budget would
/// not cover the search.
fn repair_segment(
	segment: &[Token],
	following: &Span,
	layout: &Layout,
	reported: &[usize],
	schema: Option<&SchemaCheck<'_>>,
	budget: &mut usize,
) -> Option<Option<(Candidate, SourceRepairEvidence)>> {
	let edits = candidate_edits(segment);
	*budget = budget.checked_sub(edits.len().saturating_mul(segment.len() + 1))?;
	Some(choose_reading(
		segment, following, layout, reported, schema, edits, budget,
	))
}

fn choose_reading(
	segment: &[Token],
	following: &Span,
	layout: &Layout,
	reported: &[usize],
	schema: Option<&SchemaCheck<'_>>,
	edits: Vec<Edit>,
	budget: &mut usize,
) -> Option<(Candidate, SourceRepairEvidence)> {
	let unedited = parents(&parse_segment(segment, None, following).statements);
	let mut candidates = Vec::new();
	for edit in edits {
		let parsed = parse_segment(segment, Some(edit), following);
		if !parsed.diagnostics.is_empty() || !is_one_definition(&parsed.statements) {
			continue;
		}
		let mut tokens = segment.to_vec();
		apply(&mut tokens, edit, &anchor_span(segment, edit, following));
		candidates.push(Candidate {
			edit,
			moved: moved_statements(&unedited, &parents(&parsed.statements)),
			violations: layout.violations(&tokens),
			statements: parsed.statements,
		});
	}

	// Candidates that read the same, in first-seen order.
	let mut readings: Vec<Vec<Candidate>> = Vec::new();
	for candidate in candidates {
		match readings
			.iter_mut()
			.find(|reading| same_statements(&reading[0].statements, &candidate.statements))
		{
			Some(reading) => reading.push(candidate),
			None => readings.push(vec![candidate]),
		}
	}
	if readings.len() == 1 {
		let chosen = choose_edit(readings.pop()?, segment, following, reported)?;
		return Some((chosen, SourceRepairEvidence::OnlyReading));
	}
	// The schema rules out the readings that break more of its rules than
	// another; a reading it alone leaves is the repair. Checking a definition
	// costs far more than parsing it, so only the readings that could win are
	// checked: those that move the fewest statements, and the one the
	// indentation agrees with most.
	if let Some(schema) = schema
		&& readings.len() > 1
	{
		readings.sort_by_key(|reading| reading[0].moved);
		if readings.len() > SCHEMA_CONTENDERS {
			let indented = (0..readings.len()).min_by_key(|index| {
				readings[*index]
					.iter()
					.map(|candidate| candidate.violations)
					.min()
					.unwrap_or(usize::MAX)
			})?;
			if indented >= SCHEMA_CONTENDERS {
				readings.swap(indented, SCHEMA_CONTENDERS - 1);
			}
			readings.truncate(SCHEMA_CONTENDERS);
		}
		let segment_cost = segment.len().saturating_mul(SCHEMA_COST_PER_TOKEN);
		*budget = budget.checked_sub(segment_cost.saturating_mul(readings.len()))?;
		let broken: Vec<usize> = readings
			.iter()
			.map(|reading| schema(&reading[0].statements))
			.collect();
		let fewest = broken.iter().copied().min().unwrap_or(0);
		let mut kept = Vec::new();
		for (reading, broken) in readings.into_iter().zip(broken) {
			if broken == fewest {
				kept.push(reading);
			}
		}
		readings = kept;
		if readings.len() == 1 {
			let chosen = choose_edit(readings.pop()?, segment, following, reported)?;
			return Some((chosen, SourceRepairEvidence::OnlySchemaValid));
		}
	}
	let (reading, evidence) = match readings.len() {
		0 => return None,
		1 => (readings.pop()?, SourceRepairEvidence::OnlyReading),
		_ => {
			// The reading that changes the tree least wins, if it is the only
			// one that does.
			let moved = |reading: &Vec<Candidate>| reading[0].moved;
			readings.sort_by_key(moved);
			if moved(&readings[1]) == moved(&readings[0]) {
				return None;
			}
			// The indentation only checks it. Lines no edit moves disagree with
			// every reading alike, so the indentation clearly favours a reading
			// when fewer than half as many lines disagree with it as with any
			// other; when that reading is another one, the evidence conflicts.
			let fewest = |reading: &Vec<Candidate>| {
				reading
					.iter()
					.map(|candidate| candidate.violations)
					.min()
					.unwrap_or(usize::MAX)
			};
			let winner = fewest(&readings[0]);
			if readings[1..].iter().any(|other| 2 * fewest(other) < winner) {
				return None;
			}
			(
				readings.swap_remove(0),
				SourceRepairEvidence::SmallestChange,
			)
		}
	};
	let chosen = choose_edit(reading, segment, following, reported)?;
	Some((chosen, evidence))
}

/// Of the edits that give one reading, the one the indentation agrees with
/// most, then the one where the parser first reported the error.
fn choose_edit(
	reading: Vec<Candidate>,
	segment: &[Token],
	following: &Span,
	reported: &[usize],
) -> Option<Candidate> {
	reading.into_iter().min_by_key(|candidate| {
		let offset = anchor_span(segment, candidate.edit, following).offset;
		(candidate.violations, !reported.contains(&offset))
	})
}

/// Whether a repaired segment reads as what it was cut as: one definition,
/// beside comments. An edit that leaves loose values or a second statement
/// has not found the segment's structure, however cleanly it parses.
fn is_one_definition(statements: &[AstStatement]) -> bool {
	let mut definitions = statements
		.iter()
		.filter(|statement| !matches!(statement, AstStatement::Comment { .. }));
	matches!(definitions.next(), Some(AstStatement::Assignment { .. }))
		&& definitions.next().is_none()
}

/// Each statement's parent, both identified by where they start in the text.
/// Comments are not statements of the tree.
fn parents(statements: &[AstStatement]) -> HashMap<usize, Option<usize>> {
	fn walk(
		statements: &[AstStatement],
		parent: Option<usize>,
		into: &mut HashMap<usize, Option<usize>>,
	) {
		for statement in statements {
			let (start, value) = match statement {
				AstStatement::Assignment { span, value, .. } => (span.start.offset, value),
				AstStatement::Item { span, value } => (span.start.offset, value),
				AstStatement::Comment { .. } => continue,
			};
			into.insert(start, parent);
			if let AstValue::Block { items, .. } = value {
				walk(items, Some(start), into);
			}
		}
	}
	let mut into = HashMap::new();
	walk(statements, None, &mut into);
	into
}

/// How many statements one tree has under another parent than the other, or
/// has and the other does not.
fn moved_statements(
	before: &HashMap<usize, Option<usize>>,
	after: &HashMap<usize, Option<usize>>,
) -> usize {
	let changed = after
		.iter()
		.filter(|(start, parent)| before.get(start) != Some(parent))
		.count();
	let dropped = before
		.keys()
		.filter(|start| !after.contains_key(start))
		.count();
	changed + dropped
}

/// Every one-token edit worth trying in a segment: leave out any brace, add a
/// `}` before any line, or add a `{` after an `=` that ends its line.
fn candidate_edits(segment: &[Token]) -> Vec<Edit> {
	let mut edits = Vec::new();
	for (index, token) in segment.iter().enumerate() {
		if matches!(token.kind, TokenKind::LBrace | TokenKind::RBrace) {
			edits.push(Edit::Remove(index));
		}
	}
	for index in 1..=segment.len() {
		let line_start = matches!(segment[index - 1].kind, TokenKind::Newline);
		let blank = segment
			.get(index)
			.is_some_and(|token| matches!(token.kind, TokenKind::Newline));
		if index == segment.len() || (line_start && !blank) {
			edits.push(Edit::InsertClosing(index));
		}
	}
	for (index, token) in segment.iter().enumerate() {
		if !matches!(token.kind, TokenKind::Eq) {
			continue;
		}
		let rest = segment[index + 1..]
			.iter()
			.find(|token| !matches!(token.kind, TokenKind::Comment(_)));
		if rest.is_none_or(|token| matches!(token.kind, TokenKind::Newline)) {
			edits.push(Edit::InsertOpening(index));
		}
	}
	edits
}

fn parse_segment(segment: &[Token], edit: Option<Edit>, following: &Span) -> ParsedStatements {
	let mut tokens = segment.to_vec();
	if let Some(edit) = edit {
		apply(&mut tokens, edit, &anchor_span(segment, edit, following));
	}
	tokens.push(Token {
		kind: TokenKind::Eof,
		span: SpanRange {
			start: following.clone(),
			end: following.clone(),
		},
	});
	ParserState::new(tokens).parse_file()
}

/// Where an edit's token is: the token it removes, the token a `}` goes
/// before, or the end of the `=` a `{` goes after.
fn anchor_span(tokens: &[Token], edit: Edit, following: &Span) -> Span {
	match edit {
		Edit::Remove(index) => tokens[index].span.start.clone(),
		Edit::InsertClosing(index) => tokens
			.get(index)
			.map_or_else(|| following.clone(), |token| token.span.start.clone()),
		Edit::InsertOpening(index) => tokens[index].span.end.clone(),
	}
}

/// The index of the whole-file token an edit is anchored at.
fn edit_anchor(edit: Edit) -> usize {
	match edit {
		Edit::Remove(index) | Edit::InsertClosing(index) | Edit::InsertOpening(index) => index,
	}
}

/// Applies `edit`; an added brace is zero-width at `at`, so every other
/// token keeps its place in the source text.
fn apply(tokens: &mut Vec<Token>, edit: Edit, at: &Span) {
	let synthetic = |kind| Token {
		kind,
		span: SpanRange {
			start: at.clone(),
			end: at.clone(),
		},
	};
	match edit {
		Edit::Remove(index) => {
			tokens.remove(index);
		}
		Edit::InsertClosing(index) => tokens.insert(index, synthetic(TokenKind::RBrace)),
		Edit::InsertOpening(index) => {
			let at = tokens[index].span.end.clone();
			tokens.insert(
				index + 1,
				Token {
					kind: TokenKind::LBrace,
					span: SpanRange {
						start: at.clone(),
						end: at,
					},
				},
			);
		}
	}
}

fn edit_diagnostic(
	tokens: &[Token],
	edit: Edit,
	evidence: SourceRepairEvidence,
) -> ParseDiagnostic {
	let (code, message, span, kind) = match edit {
		Edit::Remove(index) => {
			let token = &tokens[index];
			if matches!(token.kind, TokenKind::RBrace) {
				(
					ParseDiagnosticCode::UnmatchedClosingBrace,
					"unexpected closing brace without an opening block",
					token.span.clone(),
					SourceRepairEdit::RemovedClosingBrace,
				)
			} else {
				(
					ParseDiagnosticCode::UnmatchedOpeningBrace,
					"opening brace is never closed",
					token.span.clone(),
					SourceRepairEdit::RemovedOpeningBrace,
				)
			}
		}
		Edit::InsertClosing(index) => {
			let at = tokens[index].span.start.clone();
			(
				ParseDiagnosticCode::MissingClosingBrace,
				"a block is missing its closing brace before this line",
				SpanRange {
					start: at.clone(),
					end: at,
				},
				SourceRepairEdit::InsertedClosingBrace,
			)
		}
		Edit::InsertOpening(index) => {
			let at = tokens[index].span.end.clone();
			(
				ParseDiagnosticCode::MissingOpeningBrace,
				"a block value is missing its opening brace",
				SpanRange {
					start: at.clone(),
					end: at,
				},
				SourceRepairEdit::InsertedOpeningBrace,
			)
		}
	};
	repaired(
		code,
		message,
		span,
		SourceRepair {
			edit: kind,
			evidence,
		},
	)
}

fn repaired(
	code: ParseDiagnosticCode,
	message: &str,
	span: SpanRange,
	repair: SourceRepair,
) -> ParseDiagnostic {
	ParseDiagnostic {
		code,
		message: message.to_string(),
		span,
		repair: Some(repair),
	}
}

/// Whether two statement lists are the same tree, wherever their tokens are.
fn same_statements(left: &[AstStatement], right: &[AstStatement]) -> bool {
	left.len() == right.len()
		&& left
			.iter()
			.zip(right)
			.all(|(left, right)| same_statement(left, right))
}

fn same_statement(left: &AstStatement, right: &AstStatement) -> bool {
	match (left, right) {
		(
			AstStatement::Assignment {
				key: left_key,
				value: left_value,
				..
			},
			AstStatement::Assignment {
				key: right_key,
				value: right_value,
				..
			},
		) => left_key == right_key && same_value(left_value, right_value),
		(AstStatement::Item { value: left, .. }, AstStatement::Item { value: right, .. }) => {
			same_value(left, right)
		}
		(AstStatement::Comment { text: left, .. }, AstStatement::Comment { text: right, .. }) => {
			left == right
		}
		_ => false,
	}
}

fn same_value(left: &AstValue, right: &AstValue) -> bool {
	match (left, right) {
		(AstValue::Scalar { value: left, .. }, AstValue::Scalar { value: right, .. }) => {
			left == right
		}
		(AstValue::Block { items: left, .. }, AstValue::Block { items: right, .. }) => {
			same_statements(left, right)
		}
		_ => false,
	}
}

/// The indentation depth of each line in the file's own style. A file is
/// indented with tabs or with spaces, whichever more lines use; with spaces,
/// one level is the smallest run any line is indented by. A line indented in
/// the other style, or in both, says nothing about its depth.
struct Layout {
	/// By line number from 1; `None` for a line with no usable indentation.
	depths: Vec<Option<usize>>,
}

/// How one line is indented.
enum Indent {
	Blank,
	None,
	Tabs(usize),
	Spaces(usize),
	Mixed,
}

impl Indent {
	fn of(line: &str) -> Self {
		if line.trim().is_empty() {
			return Self::Blank;
		}
		let tabs = line.bytes().take_while(|byte| *byte == b'\t').count();
		let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
		let whitespace = line
			.bytes()
			.take_while(|byte| matches!(byte, b'\t' | b' '))
			.count();
		match (tabs, spaces) {
			(0, 0) => Self::None,
			(tabs, 0) if tabs == whitespace => Self::Tabs(tabs),
			(0, spaces) if spaces == whitespace => Self::Spaces(spaces),
			_ => Self::Mixed,
		}
	}
}

impl Layout {
	fn new(source: &str) -> Self {
		let indents: Vec<Indent> = source.split('\n').map(Indent::of).collect();
		let tab_lines = indents
			.iter()
			.filter(|indent| matches!(indent, Indent::Tabs(_)))
			.count();
		let space_runs = indents.iter().filter_map(|indent| match indent {
			Indent::Spaces(spaces) => Some(*spaces),
			_ => None,
		});
		let space_lines = space_runs.clone().count();
		let space_unit = space_runs.min().unwrap_or(4);
		let tabs = tab_lines >= space_lines;
		let depths = indents
			.iter()
			.map(|indent| match indent {
				Indent::None => Some(0),
				Indent::Tabs(tabs_run) if tabs => Some(*tabs_run),
				Indent::Spaces(spaces) if !tabs && spaces % space_unit == 0 => {
					Some(spaces / space_unit)
				}
				_ => None,
			})
			.collect();
		Self { depths }
	}

	fn depth(&self, line: usize) -> Option<usize> {
		self.depths.get(line.checked_sub(1)?).copied().flatten()
	}

	/// How many lines are indented other than their brace depth, where a line
	/// that starts by closing blocks is at the depth it closes to. Comment
	/// lines and lines without usable indentation are not counted.
	fn violations(&self, tokens: &[Token]) -> usize {
		let mut depth = 0usize;
		let mut violations = 0;
		let mut line_start = true;
		for (index, token) in tokens.iter().enumerate() {
			if matches!(token.kind, TokenKind::Newline) {
				line_start = true;
				continue;
			}
			if line_start {
				line_start = false;
				if !matches!(token.kind, TokenKind::Comment(_) | TokenKind::Eof) {
					let closers = tokens[index..]
						.iter()
						.take_while(|token| matches!(token.kind, TokenKind::RBrace))
						.count();
					// A brace added at the end of the file has no line to check.
					let expected = depth.saturating_sub(closers);
					if self
						.depth(token.span.start.line)
						.is_some_and(|indent| indent != expected)
					{
						violations += 1;
					}
				}
			}
			match token.kind {
				TokenKind::LBrace => depth += 1,
				TokenKind::RBrace => depth = depth.saturating_sub(1),
				_ => {}
			}
		}
		violations
	}
}

#[cfg(test)]
mod tests {
	use super::super::{
		AstStatement, AstValue, ParseDiagnosticCode, ParsedStatements, ScriptSyntax,
		parse_clausewitz_statements,
	};
	use crate::model::{SourceRepair, SourceRepairEdit, SourceRepairEvidence};

	fn parse(source: &str) -> ParsedStatements {
		parse_clausewitz_statements(ScriptSyntax::Clausewitz, source)
	}

	/// The tree without positions: `key = { ... }`, `key = value` or `value`.
	fn shape(statements: &[AstStatement]) -> String {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, value, .. } => {
					Some(format!("{key}={}", value_shape(value)))
				}
				AstStatement::Item { value, .. } => Some(value_shape(value)),
				AstStatement::Comment { .. } => None,
			})
			.collect::<Vec<_>>()
			.join(" ")
	}

	fn value_shape(value: &AstValue) -> String {
		match value {
			AstValue::Scalar { value, .. } => value.as_text(),
			AstValue::Block { items, .. } => format!("{{{}}}", shape(items)),
		}
	}

	/// The one diagnostic of `parsed`, which must be a repair.
	fn only_repair(parsed: &ParsedStatements) -> (ParseDiagnosticCode, usize, usize, SourceRepair) {
		let [diagnostic] = parsed.diagnostics.as_slice() else {
			panic!("expected one diagnostic: {:?}", parsed.diagnostics);
		};
		let repair = diagnostic
			.repair
			.unwrap_or_else(|| panic!("expected a repair: {diagnostic:?}"));
		(
			diagnostic.code,
			diagnostic.span.start.line,
			diagnostic.span.start.column,
			repair,
		)
	}

	#[test]
	fn an_unmatched_closing_brace_is_left_out_where_the_parser_found_it() {
		let parsed = parse(include_str!(
			"../../../../../tests/fixtures/cultures/malformed/extra_closing_brace.txt"
		));
		assert_eq!(
			only_repair(&parsed),
			(
				ParseDiagnosticCode::UnmatchedClosingBrace,
				5,
				1,
				SourceRepair {
					edit: SourceRepairEdit::RemovedClosingBrace,
					evidence: SourceRepairEvidence::OnlyReading,
				}
			)
		);
		assert_eq!(
			shape(&parsed.statements),
			"germanic={graphical_culture=westerngfx} iberian={graphical_culture=westerngfx}"
		);
	}

	fn not_repaired(parsed: &ParsedStatements) -> bool {
		!parsed.diagnostics.is_empty()
			&& parsed
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.repair.is_none())
	}

	#[test]
	fn a_missing_brace_goes_where_the_fewest_statements_move() {
		// Closing `a` at the end keeps every statement where the text puts
		// it; closing `b` early, or leaving out a `{`, moves some. One line
		// gives the indentation no say.
		let parsed = parse("a = { b = { c = 1 d = 2 }\n");
		let (code, _, _, repair) = only_repair(&parsed);
		assert_eq!(
			(code, repair),
			(
				ParseDiagnosticCode::MissingClosingBrace,
				SourceRepair {
					edit: SourceRepairEdit::InsertedClosingBrace,
					evidence: SourceRepairEvidence::SmallestChange,
				}
			)
		);
		assert_eq!(shape(&parsed.statements), "a={b={c=1 d=2}}");
	}

	#[test]
	fn the_fewest_moves_are_not_made_against_the_indentation() {
		// Closing `OR` at the end moves nothing, but the tabs clearly put `y`
		// beside `OR`: the evidence conflicts, so this is for review.
		let parsed = parse("a = {\n\tOR = {\n\t\tx = 1\n\ty = 2\n}\nb = {\n\tz = 3\n}\n");
		assert!(not_repaired(&parsed), "{:?}", parsed.diagnostics);
	}

	#[test]
	fn a_definition_left_open_no_longer_swallows_the_next_one() {
		let source = "a = {\n\tx = 1\nb = {\n\ty = 2\n}\n";
		let parsed = parse(source);
		let (code, line, column, repair) = only_repair(&parsed);
		assert_eq!(
			(code, line, column),
			(ParseDiagnosticCode::MissingClosingBrace, 3, 1)
		);
		assert_eq!(repair.edit, SourceRepairEdit::InsertedClosingBrace);
		assert_eq!(shape(&parsed.statements), "a={x=1} b={y=2}");
	}

	#[test]
	fn a_missing_opening_brace_against_the_fewest_moves_is_not_made() {
		// Read unedited, `OR = x = 1` is `OR = 1`, and leaving out a `}`
		// keeps that tree; adding `{` after `OR =` moves two statements but
		// is what the tabs say. The evidence conflicts.
		let parsed = parse("a = {\n\tOR =\n\t\tx = 1\n\t\ty = 2\n\t}\n}\n");
		assert!(not_repaired(&parsed), "{:?}", parsed.diagnostics);
	}

	#[test]
	fn an_unterminated_string_ends_at_its_line() {
		let parsed = parse("a = {\n\tname = \"abc\n}\nb = { c = 1 }\n");
		let (code, line, _, repair) = only_repair(&parsed);
		assert_eq!(
			(code, line, repair.edit),
			(
				ParseDiagnosticCode::UnterminatedString,
				2,
				SourceRepairEdit::ClosedStringAtLineEnd
			)
		);
		assert_eq!(shape(&parsed.statements), "a={name=abc} b={c=1}");
	}

	#[test]
	fn separate_definitions_are_repaired_one_edit_each() {
		let parsed = parse("a = {\n\tb = { c = 1 }\n}\n}\nd = {\n\te = 1\n");
		assert_eq!(parsed.diagnostics.len(), 2, "{:?}", parsed.diagnostics);
		assert!(
			parsed
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.repair.is_some()),
			"{:?}",
			parsed.diagnostics
		);
		assert_eq!(shape(&parsed.statements), "a={b={c=1}} d={e=1}");
	}

	#[test]
	fn an_edit_that_parses_but_leaves_loose_values_is_not_a_repair() {
		// Leaving out `{` parses, as `name = invalid` and four loose words.
		let parsed = parse("name { = invalid syntax with unclosed\nbraces\n");
		assert!(
			parsed
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.repair.is_none()),
			"{:?}",
			parsed.diagnostics
		);
	}

	#[test]
	fn a_definition_needing_two_edits_is_not_repaired() {
		let parsed = parse("a = {\n\tb = { c = 1 } }\n}\n}\n");
		assert!(
			parsed
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.repair.is_none()),
			"{:?}",
			parsed.diagnostics
		);
	}

	#[test]
	fn a_column_one_line_inside_a_sound_definition_does_not_cut_it() {
		// `b` is written at column 1 but belongs to `a`; only `e` is broken.
		let parsed = parse("a = {\nb = { c = 1 }\n\td = 2\n}\ne = {\n\tf = 1\n");
		let (code, line, _, _) = only_repair(&parsed);
		assert_eq!((code, line), (ParseDiagnosticCode::MissingClosingBrace, 7));
		assert_eq!(shape(&parsed.statements), "a={b={c=1} d=2} e={f=1}");
	}

	#[test]
	fn lua_is_never_repaired() {
		let parsed = parse_clausewitz_statements(ScriptSyntax::Lua, "a = { b = 1 }\n}\n");
		assert_eq!(parsed.diagnostics.len(), 1);
		assert!(parsed.diagnostics[0].repair.is_none());
	}
}
