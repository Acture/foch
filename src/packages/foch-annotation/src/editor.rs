//! Completion and hover inside annotation comments.
//!
//! Positions are UTF-8 byte offsets into the source; editors convert to their
//! own encoding at their boundary.

use crate::extract::annotation_schema;
use crate::schema::{AnnotationSchema, Registry};
use crate::value::game_path;
use foch::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};
use serde::Serialize;
use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Completion {
	pub label: String,
	pub insert_text: String,
	pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Hover {
	pub markdown: String,
	pub range: Range<usize>,
}

/// `Some`, including an empty list, claims an annotation context: callers
/// should not fall back to native-script completions inside these comments.
pub fn completions(source: &str, cursor: usize, registry: &Registry) -> Option<Vec<Completion>> {
	let (schema, context) = annotation_context(source, cursor, registry)?;
	let Some(prefix) = context.prefix else {
		return Some(Vec::new());
	};
	Some(
		schema
			.params
			.iter()
			.filter(|param| {
				param.name.starts_with(&prefix)
					&& !context.keys.iter().any(|range| {
						&source[range.clone()] == param.name
							&& !(range.start..=range.end).contains(&cursor)
					})
			})
			.map(|param| Completion {
				label: param.name.to_owned(),
				insert_text: format!("{} = {}", param.name, param.example),
				detail: format!(
					"{} ({}) — {}",
					param.value_type.describe(),
					if param.required {
						"required"
					} else {
						"optional"
					},
					param.description
				),
			})
			.collect(),
	)
}

pub fn hover(source: &str, cursor: usize, registry: &Registry) -> Option<Hover> {
	let (schema, context) = annotation_context(source, cursor, registry)?;
	let range = context
		.keys
		.into_iter()
		.find(|range| range.contains(&cursor))?;
	let param = schema.param(&source[range.clone()])?;
	Some(Hover {
		markdown: format!(
			"**{}** · `{}` · {}\n\n{}\n\n```text\n{} = {}\n```\n\n{}",
			param.name,
			param.value_type.describe(),
			if param.required {
				"required"
			} else {
				"optional"
			},
			param.description,
			param.name,
			param.example,
			schema.description
		),
		range,
	})
}

#[derive(Default)]
struct AnnotationContext {
	keys: Vec<Range<usize>>,
	prefix: Option<String>,
}

#[derive(Default)]
enum HeaderState {
	#[default]
	Key,
	KeyName(usize),
	Equals,
	Value,
	Scalar,
	AfterValue,
}

#[derive(Default)]
struct HeaderScanner {
	context: AnnotationContext,
	state: HeaderState,
	braces: usize,
	quoted: bool,
	escaped: bool,
}

impl HeaderScanner {
	fn capture(&mut self, source: &str, offset: usize, cursor: usize) {
		if offset != cursor || self.braces != 0 || self.quoted {
			return;
		}
		self.context.prefix = match self.state {
			HeaderState::Key => Some(String::new()),
			HeaderState::KeyName(start) => Some(source[start..offset].to_owned()),
			_ => None,
		};
	}

	/// Scan only original comment contents; offsets always remain source offsets.
	fn line(&mut self, source: &str, range: Range<usize>, cursor: usize) -> Option<usize> {
		for offset in range.clone() {
			self.capture(source, offset, cursor);
			let byte = source.as_bytes()[offset];
			if self.quoted {
				if self.escaped {
					self.escaped = false;
				} else if byte == b'\\' {
					self.escaped = true;
				} else if byte == b'"' {
					self.quoted = false;
				}
				continue;
			}
			match byte {
				b'#' => {
					self.end_line();
					return None;
				}
				b'"' => {
					self.quoted = true;
					self.state = HeaderState::AfterValue;
					continue;
				}
				b'{' => {
					self.braces += 1;
					self.state = HeaderState::AfterValue;
					continue;
				}
				b'}' => {
					self.braces = self.braces.saturating_sub(1);
					continue;
				}
				b')' if self.braces == 0 => return Some(offset),
				_ if self.braces > 0 => continue,
				_ => {}
			}
			match self.state {
				HeaderState::Key if byte.is_ascii_alphabetic() || byte == b'_' => {
					self.context.keys.push(offset..offset + 1);
					self.state = HeaderState::KeyName(offset);
				}
				HeaderState::KeyName(_) if byte.is_ascii_alphanumeric() || byte == b'_' => {
					if let Some(key) = self.context.keys.last_mut() {
						key.end = offset + 1;
					}
				}
				HeaderState::KeyName(_) | HeaderState::Equals => {
					self.state = if byte == b'=' {
						HeaderState::Value
					} else {
						HeaderState::Equals
					};
				}
				HeaderState::Value if !byte.is_ascii_whitespace() => {
					self.state = HeaderState::Scalar
				}
				HeaderState::Scalar | HeaderState::AfterValue
					if byte.is_ascii_whitespace() || byte == b',' =>
				{
					self.state = HeaderState::Key;
				}
				_ => {}
			}
		}
		self.capture(source, range.end, cursor);
		self.end_line();
		None
	}

	fn end_line(&mut self) {
		match self.state {
			HeaderState::KeyName(_) => self.state = HeaderState::Equals,
			HeaderState::Scalar | HeaderState::AfterValue => self.state = HeaderState::Key,
			_ => {}
		}
	}
}

fn annotation_context(
	source: &str,
	cursor: usize,
	registry: &Registry,
) -> Option<(&'static AnnotationSchema, AnnotationContext)> {
	// The native AST prevents comment-looking text in multiline strings from
	// becoming annotations, without changing the grammar or source text.
	let parsed = parse_clausewitz_content(game_path("editor.txt"), source);
	let statements = &parsed.ast.statements;
	let mut index = 0;
	while index < statements.len() {
		let AstStatement::Comment { text, span } = &statements[index] else {
			index += 1;
			continue;
		};
		let Some(schema) = annotation_schema(text, registry).filter(|schema| {
			text.trim_start()
				.strip_prefix(schema.name)
				.is_some_and(|rest| rest.trim_start().starts_with('('))
		}) else {
			index += 1;
			continue;
		};
		let mut scanner = HeaderScanner::default();
		let first = index;
		let mut contains_cursor = false;
		while let Some(AstStatement::Comment { span: comment, .. }) = statements.get(index) {
			let line_start = source[..comment.start.offset]
				.rfind('\n')
				.map_or(0, |offset| offset + 1);
			if !source[line_start..comment.start.offset].trim().is_empty() {
				break;
			}
			let start = if index == first {
				comment.start.offset
					+ source[comment.start.offset..comment.end.offset].find('(')?
					+ 1
			} else {
				comment.start.offset + 1
			};
			index += 1;
			let close = scanner.line(source, start..comment.end.offset, cursor);
			let end = close.unwrap_or(comment.end.offset);
			contains_cursor |= (start..=end).contains(&cursor);
			if close.is_some() {
				break;
			}
		}
		if contains_cursor {
			return Some((schema, scanner.context));
		}
		if index == first {
			index += 1;
		}
		if span.start.offset > cursor {
			break;
		}
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::builtin::REGISTRY;

	fn cursor(marked: &str) -> (String, usize) {
		let offset = marked.find('|').expect("cursor marker");
		(marked.replacen('|', "", 1), offset)
	}

	#[test]
	fn completes_parameters_of_the_annotation_under_the_cursor() {
		let (source, offset) = cursor("#test(time=1444.11.11, ta|)");
		let items = completions(&source, offset, &REGISTRY).expect("annotation completions");
		assert_eq!(items.len(), 1);
		assert_eq!(items[0].label, "tag");
		assert!(items[0].insert_text.contains("SWE"));
		assert!(items[0].detail.contains("country tag"));

		let (source, offset) = cursor("#xfail(reason=\"x\", st|)");
		let items = completions(&source, offset, &REGISTRY).expect("modifier completions");
		assert_eq!(
			items
				.iter()
				.map(|item| item.label.as_str())
				.collect::<Vec<_>>(),
			["strict"]
		);
	}

	#[test]
	fn continues_after_multiline_blocks_without_repeating_parameters() {
		let (source, offset) = cursor("#test(time=1444.11.11,\n# effect={ log=\"😀 )\" },\n# |)");
		let items = completions(&source, offset, &REGISTRY).expect("continuation completions");
		assert!(items.iter().any(|item| item.label == "tag"));
		assert!(
			!items
				.iter()
				.any(|item| item.label == "time" || item.label == "effect")
		);
	}

	#[test]
	fn continues_after_a_native_comment_inside_the_annotation() {
		let (source, offset) = cursor("#test(time=1444.11.11# initial date\n#ta|)");
		let items = completions(&source, offset, &REGISTRY).expect("annotation continuation");
		assert_eq!(items.len(), 1);
		assert_eq!(items[0].label, "tag");
	}

	#[test]
	fn does_not_complete_inside_values_or_native_blocks() {
		for marked in [
			"#test(time=|)",
			"#test(name=\"ta|\")",
			"#test(expect={ ta| })",
			"#test(effect={\n# ta|\n# })",
			"#test(time=1444.11.11 # ta|\n# )",
		] {
			let (source, offset) = cursor(marked);
			assert!(
				completions(&source, offset, &REGISTRY)
					.expect("annotation context")
					.is_empty(),
				"{marked}"
			);
			assert!(hover(&source, offset, &REGISTRY).is_none(), "{marked}");
		}
	}

	#[test]
	fn ignores_ordinary_comments_strings_inline_comments_and_script() {
		for marked in [
			"# ordinary #test(ta|)",
			"name=\"#test(ta|)\"",
			"name=\"multiline\n#test(ta|)\n\"",
			"name=yes #test(ta|)",
			"#testing(ta|)",
			"#test(time=1444.11.11)\nta|",
			"#test(time=1444.11.11)\n# ta|",
		] {
			let (source, offset) = cursor(marked);
			assert!(
				completions(&source, offset, &REGISTRY).is_none(),
				"{marked}"
			);
			assert!(hover(&source, offset, &REGISTRY).is_none(), "{marked}");
		}
	}

	#[test]
	fn hover_uses_schema_metadata_and_byte_ranges() {
		let (source, offset) = cursor("#test(name=\"😀\", ta|g=SWE)");
		let info = hover(&source, offset, &REGISTRY).expect("parameter hover");
		assert!(info.markdown.contains("country tag"));
		// tag is required unless #parametrize supplies it, so the schema marks
		// it optional and the description states the real rule.
		assert!(info.markdown.contains("optional"));
		assert!(info.markdown.contains("SWE"));
		assert!(info.markdown.contains("hidden=yes"));
		assert_eq!(&source[info.range], "tag");
	}
}
