//! Conversions between UTF-16 editor positions and UTF-8 source byte offsets.

use crate::game::eu4::script::parser::SpanRange;

use super::schema::{EditorPosition, EditorRange};

/// Convert a zero-based UTF-16 position to a source byte offset.
///
/// Line terminators are excluded from character positions. Positions past the
/// line's content or inside a surrogate pair are invalid. A BOM, when present,
/// counts as an ordinary character in the supplied editor text.
pub fn byte_offset(text: &str, position: EditorPosition) -> Option<usize> {
	let mut line_start = 0;
	for _ in 0..position.line {
		line_start += text[line_start..].find('\n')? + 1;
	}
	let remainder = &text[line_start..];
	let line = match remainder.split_once('\n') {
		Some((line, _)) => line.strip_suffix('\r').unwrap_or(line),
		None => remainder,
	};
	let target = position.character as usize;
	let mut character = 0;
	for (offset, value) in line.char_indices() {
		if character == target {
			return Some(line_start + offset);
		}
		character += value.len_utf16();
		if character > target {
			return None;
		}
	}
	(character == target).then_some(line_start + line.len())
}

/// Convert a UTF-8 source byte boundary to a zero-based UTF-16 position.
///
/// Offsets inside a UTF-8 character or between the CR and LF of a line
/// terminator are invalid. The start of a line terminator is the line's end.
pub fn position_at(text: &str, offset: usize) -> Option<EditorPosition> {
	let prefix = text.get(..offset)?;
	if prefix.ends_with('\r') && text.as_bytes().get(offset) == Some(&b'\n') {
		return None;
	}
	let line_start = prefix.rfind('\n').map_or(0, |offset| offset + 1);
	Some(EditorPosition {
		line: u32::try_from(prefix.bytes().filter(|&byte| byte == b'\n').count()).ok()?,
		character: u32::try_from(prefix[line_start..].encode_utf16().count()).ok()?,
	})
}

/// Map a parser's half-open byte span to an editor range in the original text.
pub fn range_from_span(text: &str, span: &SpanRange) -> Option<EditorRange> {
	if span.start.offset > span.end.offset {
		return None;
	}
	Some(EditorRange {
		start: position_at(text, span.start.offset)?,
		end: position_at(text, span.end.offset)?,
	})
}

#[cfg(test)]
mod tests {
	use crate::game::eu4::script::parser::Span;

	use super::*;

	fn position(line: u32, character: u32) -> EditorPosition {
		EditorPosition { line, character }
	}

	#[test]
	fn positions_round_trip_unicode_bom_crlf_and_trailing_empty_line() {
		let text = "\u{feff}中😀x\r\n文\n";
		for (offset, expected) in [
			(0, position(0, 0)),
			(3, position(0, 1)),
			(6, position(0, 2)),
			(10, position(0, 4)),
			(11, position(0, 5)),
			(13, position(1, 0)),
			(16, position(1, 1)),
			(17, position(2, 0)),
		] {
			assert_eq!(byte_offset(text, expected), Some(offset), "{expected:?}");
			assert_eq!(position_at(text, offset), Some(expected), "{offset}");
		}
	}

	#[test]
	fn byte_offsets_reject_surrogate_interiors_and_positions_beyond_line_content() {
		let text = "\u{feff}中😀x\r\n文\n";
		for invalid in [
			position(0, 3),
			position(0, 6),
			position(1, 2),
			position(2, 1),
			position(3, 0),
			position(u32::MAX, u32::MAX),
		] {
			assert_eq!(byte_offset(text, invalid), None, "{invalid:?}");
		}
	}

	#[test]
	fn positions_reject_utf8_interiors_crlf_interiors_and_out_of_bounds_offsets() {
		let text = "\u{feff}中😀x\r\n文\n";
		for invalid in [1, 2, 4, 5, 7, 8, 9, 12, 14, 15, 18, usize::MAX] {
			assert_eq!(position_at(text, invalid), None, "{invalid}");
		}
	}

	#[test]
	fn empty_document_has_one_valid_position() {
		assert_eq!(byte_offset("", position(0, 0)), Some(0));
		assert_eq!(position_at("", 0), Some(position(0, 0)));
		assert_eq!(byte_offset("", position(0, 1)), None);
		assert_eq!(byte_offset("", position(1, 0)), None);
		assert_eq!(position_at("", 1), None);
	}

	#[test]
	fn ranges_use_source_offsets_and_keep_the_end_exclusive() {
		let text = "\u{feff}中😀x\r\n文\n";
		let span = SpanRange {
			start: Span {
				line: 99,
				column: 99,
				offset: 6,
			},
			end: Span {
				line: 99,
				column: 99,
				offset: 10,
			},
		};
		assert_eq!(
			range_from_span(text, &span),
			Some(EditorRange {
				start: position(0, 2),
				end: position(0, 4),
			})
		);
		let reversed = SpanRange {
			start: span.end.clone(),
			end: span.start.clone(),
		};
		assert_eq!(range_from_span(text, &reversed), None);
		let invalid_boundary = SpanRange {
			end: Span {
				offset: 7,
				..span.end
			},
			..span
		};
		assert_eq!(range_from_span(text, &invalid_boundary), None);
	}
}
