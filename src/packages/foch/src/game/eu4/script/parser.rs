use crate::model::{GamePath, GamePathBuf, SourceRepair};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod recovery;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Span {
	pub line: usize,
	pub column: usize,
	pub offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SpanRange {
	pub start: Span,
	pub end: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ScalarValue {
	Identifier(String),
	String(String),
	Number(String),
	Bool(bool),
}

impl ScalarValue {
	pub fn as_text(&self) -> String {
		match self {
			Self::Identifier(value) => value.clone(),
			Self::String(value) => value.clone(),
			Self::Number(value) => value.clone(),
			Self::Bool(value) => {
				if *value {
					"yes".to_string()
				} else {
					"no".to_string()
				}
			}
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AstValue {
	Scalar {
		value: ScalarValue,
		span: SpanRange,
	},
	Block {
		items: Vec<AstStatement>,
		span: SpanRange,
	},
}

impl AstValue {
	pub fn span(&self) -> &SpanRange {
		match self {
			Self::Scalar { span, .. } => span,
			Self::Block { span, .. } => span,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AstStatement {
	Assignment {
		key: String,
		key_span: SpanRange,
		value: AstValue,
		span: SpanRange,
	},
	Item {
		value: AstValue,
		span: SpanRange,
	},
	Comment {
		text: String,
		span: SpanRange,
	},
}

/// A parsed script identified by the game path it is loaded at.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AstFile {
	pub path: GamePathBuf,
	pub statements: Vec<AstStatement>,
}

/// What a parse diagnostic found. Consumers decide on the code, never on the
/// message text, which is for people only.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseDiagnosticCode {
	/// The file could not be read; nothing was parsed.
	ReadFailure,
	/// A Lua block comment runs to the end of the file.
	UnterminatedLuaBlockComment,
	/// A block is still open where it should have closed.
	MissingClosingBrace,
	/// A `}` that no block opened. Read alone, the parser skips one at the
	/// outermost level and keeps every statement around it.
	UnmatchedClosingBrace,
	/// A `{` that no `}` closes.
	UnmatchedOpeningBrace,
	/// A block value whose `{` is missing.
	MissingOpeningBrace,
	/// A string whose closing quote is missing, so it runs to the end of the
	/// file.
	UnterminatedString,
	/// A token that cannot start a statement; it is skipped.
	InvalidStatementStart,
	/// A token that cannot be a value; it is read as an empty identifier.
	InvalidValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseDiagnostic {
	pub code: ParseDiagnosticCode,
	pub message: String,
	pub span: SpanRange,
	/// The edit the parsed statements already include to get past this error,
	/// or `None` when they cannot be trusted. The text is never changed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub repair: Option<SourceRepair>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
	pub ast: AstFile,
	pub diagnostics: Vec<ParseDiagnostic>,
}

/// The statements of a script before it is given a game path. This is all the
/// parser itself produces: which file a script is, and so how content
/// families and rules apply to it, is decided by the caller that knows the
/// root it was loaded from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParsedStatements {
	pub statements: Vec<AstStatement>,
	pub diagnostics: Vec<ParseDiagnostic>,
}

impl ParsedStatements {
	/// Identifies the statements as the script loaded at `path`.
	pub fn into_parse_result(self, path: GamePathBuf) -> ParseResult {
		ParseResult {
			ast: AstFile {
				path,
				statements: self.statements,
			},
			diagnostics: self.diagnostics,
		}
	}
}

/// The grammar a script is read with. The game reads `.lua` files (defines
/// and random-map tweaks) with Lua comments and everything else as plain
/// Clausewitz script.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ScriptSyntax {
	Clausewitz,
	Lua,
}

impl ScriptSyntax {
	/// The syntax of a file with this extension; `lua` matches in any case.
	pub fn from_extension(extension: Option<&str>) -> Self {
		if extension.is_some_and(|extension| extension.eq_ignore_ascii_case("lua")) {
			Self::Lua
		} else {
			Self::Clausewitz
		}
	}

	pub fn for_game_path(path: &GamePath) -> Self {
		Self::from_extension(path.extension())
	}

	/// The syntax of a physical file, for callers that read a file without
	/// knowing the root it would be loaded from.
	pub fn for_physical_path(path: &Path) -> Self {
		Self::from_extension(path.extension().and_then(|extension| extension.to_str()))
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
	Identifier(String),
	String(String),
	Number(String),
	Bool(bool),
	Eq,
	LBrace,
	RBrace,
	Comment(String),
	Newline,
	Comma,
	Eof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
	kind: TokenKind,
	span: SpanRange,
}

struct Lexer<'a> {
	source: &'a str,
	bytes: &'a [u8],
	index: usize,
	line: usize,
	column: usize,
	lua_mode: bool,
	/// The opening quote of a string to end at its line's end instead of
	/// running to the end of the file.
	close_string_at_line_end: Option<usize>,
	diagnostics: Vec<ParseDiagnostic>,
}

impl<'a> Lexer<'a> {
	fn new(source: &'a str, lua_mode: bool) -> Self {
		Self {
			source,
			bytes: source.as_bytes(),
			index: 0,
			line: 1,
			column: 1,
			lua_mode,
			close_string_at_line_end: None,
			diagnostics: Vec::new(),
		}
	}

	fn take_diagnostics(&mut self) -> Vec<ParseDiagnostic> {
		std::mem::take(&mut self.diagnostics)
	}

	fn next_token(&mut self) -> Token {
		self.skip_inline_whitespace();
		let start = self.current_span();
		let Some(byte) = self.peek_byte() else {
			return Token {
				kind: TokenKind::Eof,
				span: SpanRange {
					start: start.clone(),
					end: start,
				},
			};
		};

		match byte {
			b'\n' => {
				self.advance_byte();
				Token {
					kind: TokenKind::Newline,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'=' => {
				self.advance_byte();
				Token {
					kind: TokenKind::Eq,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'{' => {
				self.advance_byte();
				Token {
					kind: TokenKind::LBrace,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'}' => {
				self.advance_byte();
				Token {
					kind: TokenKind::RBrace,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b',' if self.lua_mode => {
				self.advance_byte();
				Token {
					kind: TokenKind::Comma,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'#' => {
				self.advance_byte();
				let text_start = self.index;
				while let Some(next) = self.peek_byte() {
					if next == b'\n' {
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].trim().to_string();
				Token {
					kind: TokenKind::Comment(text),
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'-' if self.lua_mode && self.peek_byte_at(1) == Some(b'-') => {
				self.advance_byte();
				self.advance_byte();
				let text_start = self.index;
				self.consume_lua_comment_body(start.clone());
				let text = self.source[text_start..self.index].trim().to_string();
				Token {
					kind: TokenKind::Comment(text),
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'"' => {
				let quote = self.index;
				let close_at_line_end = self.close_string_at_line_end == Some(quote);
				self.advance_byte();
				let text_start = self.index;
				while let Some(next) = self.peek_byte() {
					if next == b'"' || (close_at_line_end && matches!(next, b'\r' | b'\n')) {
						break;
					}
					if next == b'\\' {
						self.advance_byte();
						if self.peek_byte().is_some() {
							self.advance_byte();
						}
						continue;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].to_string();
				if self.peek_byte() == Some(b'"') {
					self.advance_byte();
				} else if self.peek_byte().is_none() {
					self.diagnostics.push(ParseDiagnostic {
						code: ParseDiagnosticCode::UnterminatedString,
						message: "string has no closing quote before end of file".to_string(),
						span: SpanRange {
							start: start.clone(),
							end: self.current_span(),
						},
						repair: None,
					});
				}
				Token {
					kind: TokenKind::String(text),
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			b'-' | b'0'..=b'9' => {
				let text_start = self.index;
				let digit_leading = byte.is_ascii_digit();
				self.advance_byte();
				if !digit_leading
					&& !self
						.peek_byte()
						.is_some_and(|next| next.is_ascii_digit() || next == b'.')
				{
					return Token {
						kind: TokenKind::Number("-".to_string()),
						span: SpanRange {
							start: start.clone(),
							end: self.current_span(),
						},
					};
				}
				while let Some(next) = self.peek_byte() {
					if is_token_delimiter(next)
						|| (self.lua_mode && next == b',')
						|| (self.lua_mode && next == b'-' && self.peek_byte_at(1) == Some(b'-'))
					{
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].to_string();
				Token {
					kind: if is_number_token(&text) {
						TokenKind::Number(text)
					} else {
						TokenKind::Identifier(text)
					},
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
			_ => {
				let text_start = self.index;
				self.advance_byte();
				while let Some(next) = self.peek_byte() {
					if is_token_delimiter(next) || (self.lua_mode && next == b',') {
						break;
					}
					if self.lua_mode && next == b'-' && self.peek_byte_at(1) == Some(b'-') {
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].trim().to_string();
				// Only the exact lowercase spellings are boolean. `CToken::GetBool`
				// in the game is a case-sensitive `strncmp` against `yes`, so `YES`
				// is not true there; folding it here and emitting `yes` would turn
				// a false into a true in merged output.
				let kind = match text.as_str() {
					"yes" => TokenKind::Bool(true),
					"no" => TokenKind::Bool(false),
					_ => TokenKind::Identifier(text),
				};
				Token {
					kind,
					span: SpanRange {
						start: start.clone(),
						end: self.current_span(),
					},
				}
			}
		}
	}

	fn skip_inline_whitespace(&mut self) {
		while let Some(byte) = self.peek_byte() {
			if byte == b' ' || byte == b'\t' || byte == b'\r' {
				self.advance_byte();
			} else {
				break;
			}
		}
	}

	fn peek_byte(&self) -> Option<u8> {
		self.bytes.get(self.index).copied()
	}

	fn peek_byte_at(&self, offset: usize) -> Option<u8> {
		self.bytes.get(self.index + offset).copied()
	}

	fn consume_lua_comment_body(&mut self, start: Span) {
		// Caller has already consumed the opening `--`.
		// Detect block comment opener `[=*[` (Lua long-bracket comment).
		if self.peek_byte() == Some(b'[') {
			let mut probe_index = self.index;
			probe_index += 1;
			let mut level: usize = 0;
			while self.bytes.get(probe_index).copied() == Some(b'=') {
				level += 1;
				probe_index += 1;
			}
			if self.bytes.get(probe_index).copied() == Some(b'[') {
				// Confirmed block comment open: --[<eq*>[
				// Advance past the opener.
				self.advance_byte(); // first '['
				for _ in 0..level {
					self.advance_byte();
				}
				self.advance_byte(); // second '['

				loop {
					let Some(byte) = self.peek_byte() else {
						self.diagnostics.push(ParseDiagnostic {
							code: ParseDiagnosticCode::UnterminatedLuaBlockComment,
							message: format!(
								"unterminated Lua block comment --[{}[",
								"=".repeat(level)
							),
							span: SpanRange {
								start: start.clone(),
								end: self.current_span(),
							},
							repair: None,
						});
						return;
					};
					if byte == b']' {
						let mut close_probe = self.index + 1;
						let mut close_level: usize = 0;
						while self.bytes.get(close_probe).copied() == Some(b'=') {
							close_level += 1;
							close_probe += 1;
						}
						if close_level == level
							&& self.bytes.get(close_probe).copied() == Some(b']')
						{
							self.advance_byte();
							for _ in 0..level {
								self.advance_byte();
							}
							self.advance_byte();
							return;
						}
						self.advance_byte();
					} else {
						self.advance_byte();
					}
				}
			}
		}

		// Plain `--` line comment: consume until newline.
		while let Some(next) = self.peek_byte() {
			if next == b'\n' {
				break;
			}
			self.advance_byte();
		}
	}

	fn advance_byte(&mut self) {
		if let Some(byte) = self.peek_byte() {
			self.index += 1;
			if byte == b'\n' {
				self.line += 1;
				self.column = 1;
			} else {
				self.column += 1;
			}
		}
	}

	fn current_span(&self) -> Span {
		Span {
			line: self.line,
			column: self.column,
			offset: self.index,
		}
	}
}

fn is_token_delimiter(byte: u8) -> bool {
	matches!(
		byte,
		b' ' | b'\t' | b'\r' | b'\n' | b'=' | b'{' | b'}' | b'#'
	)
}

fn is_number_token(text: &str) -> bool {
	if text == "-" {
		return true;
	}
	let unsigned = text.strip_prefix('-').unwrap_or(text);
	let Some(exponent_index) = unsigned.find(['e', 'E']) else {
		return unsigned.bytes().any(|byte| byte.is_ascii_digit())
			&& unsigned
				.bytes()
				.all(|byte| byte.is_ascii_digit() || byte == b'.');
	};
	let (mantissa, exponent_with_marker) = unsigned.split_at(exponent_index);
	let exponent = &exponent_with_marker[1..];
	decimal_mantissa_is_valid(mantissa)
		&& signed_digits_are_valid(exponent)
		&& !exponent.bytes().any(|byte| matches!(byte, b'e' | b'E'))
}

fn decimal_mantissa_is_valid(mantissa: &str) -> bool {
	match mantissa.split_once('.') {
		Some((whole, fraction)) => {
			!fraction.is_empty()
				&& whole.bytes().all(|byte| byte.is_ascii_digit())
				&& fraction.bytes().all(|byte| byte.is_ascii_digit())
		}
		None => !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit()),
	}
}

fn signed_digits_are_valid(value: &str) -> bool {
	let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
	!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

struct ParserState {
	tokens: Vec<Token>,
	index: usize,
	diagnostics: Vec<ParseDiagnostic>,
}

impl ParserState {
	fn new(tokens: Vec<Token>) -> Self {
		Self {
			tokens,
			index: 0,
			diagnostics: Vec::new(),
		}
	}

	fn parse_file(mut self) -> ParsedStatements {
		let statements = self.parse_statements(false);
		ParsedStatements {
			statements,
			diagnostics: self.diagnostics,
		}
	}

	fn parse_statements(&mut self, stop_at_rbrace: bool) -> Vec<AstStatement> {
		let mut statements = Vec::new();

		loop {
			let token = self.peek();
			match &token.kind {
				TokenKind::Eof => {
					if stop_at_rbrace {
						let span = token.span.clone();
						self.diagnostics.push(ParseDiagnostic {
							code: ParseDiagnosticCode::MissingClosingBrace,
							message: "missing closing brace before end of file".into(),
							span,
							repair: None,
						});
					}
					break;
				}
				TokenKind::RBrace if stop_at_rbrace => {
					self.bump();
					break;
				}
				TokenKind::RBrace => {
					let span = token.span.clone();
					self.bump();
					self.diagnostics.push(ParseDiagnostic {
						code: ParseDiagnosticCode::UnmatchedClosingBrace,
						message: "unexpected closing brace without an opening block".into(),
						span,
						repair: None,
					});
				}
				TokenKind::Newline | TokenKind::Comma => {
					self.bump();
				}
				TokenKind::Comment(text) => {
					let span = token.span.clone();
					let text = text.clone();
					self.bump();
					statements.push(AstStatement::Comment { text, span });
				}
				_ => {
					if let Some(stmt) = self.parse_statement() {
						statements.push(stmt);
					}
				}
			}
		}

		statements
	}

	fn parse_statement(&mut self) -> Option<AstStatement> {
		let first = self.bump();
		match first.kind {
			TokenKind::Identifier(key) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					self.bump();
					let value = self.parse_value();
					let end = value.span().end.clone();
					Some(AstStatement::Assignment {
						key,
						key_span: first.span.clone(),
						value,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else if matches!(self.peek().kind, TokenKind::LBrace) {
					let value = self.parse_value();
					let end = value.span().end.clone();
					Some(AstStatement::Assignment {
						key,
						key_span: first.span.clone(),
						value,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else {
					let value = AstValue::Scalar {
						value: ScalarValue::Identifier(key),
						span: first.span.clone(),
					};
					Some(AstStatement::Item {
						value,
						span: SpanRange {
							start: first.span.start,
							end: first.span.end,
						},
					})
				}
			}
			TokenKind::String(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					let _ = self.bump();
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: value,
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else if matches!(self.peek().kind, TokenKind::LBrace) {
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: value,
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else {
					Some(AstStatement::Item {
						value: AstValue::Scalar {
							value: ScalarValue::String(value),
							span: first.span.clone(),
						},
						span: first.span,
					})
				}
			}
			TokenKind::Number(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					let _ = self.bump();
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: value,
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else if matches!(self.peek().kind, TokenKind::LBrace) {
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: value,
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else {
					Some(AstStatement::Item {
						value: AstValue::Scalar {
							value: ScalarValue::Number(value),
							span: first.span.clone(),
						},
						span: first.span,
					})
				}
			}
			TokenKind::Bool(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					let _ = self.bump();
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: if value {
							"yes".to_string()
						} else {
							"no".to_string()
						},
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else if matches!(self.peek().kind, TokenKind::LBrace) {
					let value_node = self.parse_value();
					let end = value_node.span().end.clone();
					Some(AstStatement::Assignment {
						key: if value {
							"yes".to_string()
						} else {
							"no".to_string()
						},
						key_span: first.span.clone(),
						value: value_node,
						span: SpanRange {
							start: first.span.start.clone(),
							end,
						},
					})
				} else {
					Some(AstStatement::Item {
						value: AstValue::Scalar {
							value: ScalarValue::Bool(value),
							span: first.span.clone(),
						},
						span: first.span,
					})
				}
			}
			TokenKind::LBrace => {
				let start = first.span.start;
				let items = self.parse_statements(true);
				let end = self.previous().span.end.clone();
				Some(AstStatement::Item {
					value: AstValue::Block {
						items,
						span: SpanRange {
							start: start.clone(),
							end: end.clone(),
						},
					},
					span: SpanRange { start, end },
				})
			}
			TokenKind::RBrace | TokenKind::Eof => None,
			_ => {
				self.diagnostics.push(ParseDiagnostic {
					code: ParseDiagnosticCode::InvalidStatementStart,
					message: "could not parse statement start token".to_string(),
					span: first.span,
					repair: None,
				});
				None
			}
		}
	}

	fn parse_value(&mut self) -> AstValue {
		let mut token = self.bump();
		while matches!(
			token.kind,
			TokenKind::Newline | TokenKind::Comma | TokenKind::Comment(_) | TokenKind::Eq
		) {
			token = self.bump();
		}
		match token.kind {
			TokenKind::LBrace => {
				let start = token.span.start;
				let items = self.parse_statements(true);
				let end = self.previous().span.end.clone();
				AstValue::Block {
					items,
					span: SpanRange { start, end },
				}
			}
			TokenKind::Identifier(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					self.bump();
					self.parse_value()
				} else {
					AstValue::Scalar {
						value: ScalarValue::Identifier(value),
						span: token.span,
					}
				}
			}
			TokenKind::String(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					self.bump();
					self.parse_value()
				} else {
					AstValue::Scalar {
						value: ScalarValue::String(value),
						span: token.span,
					}
				}
			}
			TokenKind::Number(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					self.bump();
					self.parse_value()
				} else {
					AstValue::Scalar {
						value: ScalarValue::Number(value),
						span: token.span,
					}
				}
			}
			TokenKind::Bool(value) => {
				if matches!(self.peek().kind, TokenKind::Eq) {
					self.bump();
					self.parse_value()
				} else {
					AstValue::Scalar {
						value: ScalarValue::Bool(value),
						span: token.span,
					}
				}
			}
			TokenKind::Comment(text) => AstValue::Scalar {
				value: ScalarValue::Identifier(text),
				span: token.span,
			},
			_ => {
				self.diagnostics.push(ParseDiagnostic {
					code: ParseDiagnosticCode::InvalidValue,
					message: "value parse failed; downgraded to empty identifier".to_string(),
					span: token.span.clone(),
					repair: None,
				});
				AstValue::Scalar {
					value: ScalarValue::Identifier("<parse-error>".to_string()),
					span: token.span,
				}
			}
		}
	}

	fn peek(&self) -> &Token {
		self.tokens.get(self.index).unwrap_or_else(|| {
			self.tokens
				.last()
				.expect("token stream should always contain eof")
		})
	}

	fn previous(&self) -> &Token {
		if self.index == 0 {
			self.tokens
				.first()
				.expect("token stream should always contain eof")
		} else {
			&self.tokens[self.index - 1]
		}
	}

	fn bump(&mut self) -> Token {
		let token = self.peek().clone();
		if !matches!(token.kind, TokenKind::Eof) {
			self.index += 1;
		}
		token
	}
}

/// Reads and parses a physical file without giving it a game path, for
/// callers that hold a file but not the root it would be loaded from. The
/// syntax follows the file's extension; a read failure is a diagnostic.
pub fn parse_clausewitz_file(path: &Path) -> ParsedStatements {
	match std::fs::read(path) {
		Ok(bytes) => {
			let content = crate::game::eu4::text::decode_paradox_bytes(&bytes);
			parse_clausewitz_statements(ScriptSyntax::for_physical_path(path), &content)
		}
		Err(err) => read_failure(&err),
	}
}

/// The result for a script that could not be read: no statements and one
/// diagnostic at the start of the file.
pub(crate) fn read_failure(err: &std::io::Error) -> ParsedStatements {
	let start = Span {
		line: 1,
		column: 1,
		offset: 0,
	};
	ParsedStatements {
		statements: Vec::new(),
		diagnostics: vec![ParseDiagnostic {
			code: ParseDiagnosticCode::ReadFailure,
			message: format!("failed to read file: {err}"),
			span: SpanRange {
				start: start.clone(),
				end: start,
			},
			repair: None,
		}],
	}
}

/// Parses the script loaded at `path`; the syntax follows its extension and
/// the AST carries the path. Errors are repaired without the schema; a mod's
/// files are read through `parse_cache`, which repairs them under the schema
/// for their path.
pub fn parse_clausewitz_content(path: &GamePath, content: &str) -> ParseResult {
	parse_clausewitz_statements(ScriptSyntax::for_game_path(path), content)
		.into_parse_result(path.to_owned())
}

/// Parses script text in `syntax` without identifying which file it is.
///
/// A Clausewitz script with errors is repaired where one small edit has a
/// single trustworthy reading; see [`recovery`]. A `.lua` file is read by a
/// Lua interpreter, which rejects the whole file instead, so it is not.
///
/// No schema is known here; a caller that knows which file the text is passes
/// one to [`recover_clausewitz_statements`] instead.
pub fn parse_clausewitz_statements(syntax: ScriptSyntax, content: &str) -> ParsedStatements {
	recover_clausewitz_statements(
		syntax,
		content,
		parse_unrecovered_statements(syntax, content),
		None,
	)
}

/// Parses script text exactly as written, with no repair.
pub fn parse_unrecovered_statements(syntax: ScriptSyntax, content: &str) -> ParsedStatements {
	let (tokens, lexer_diagnostics) = lex(content, syntax == ScriptSyntax::Lua, None);
	let mut result = ParserState::new(tokens).parse_file();
	result.diagnostics.extend(lexer_diagnostics);
	result
}

/// How many of one definition's values have a shape the schema for the file
/// being read rejects.
pub type SchemaCheck<'a> = dyn Fn(&[AstStatement]) -> usize + 'a;

/// Repairs `parsed`, the unrecovered parse of `content`, where it can. A
/// schema check, when given, rules out repairs whose definition has more
/// values of a rejected shape than another repair's.
pub fn recover_clausewitz_statements(
	syntax: ScriptSyntax,
	content: &str,
	parsed: ParsedStatements,
	schema: Option<&SchemaCheck<'_>>,
) -> ParsedStatements {
	if syntax == ScriptSyntax::Clausewitz && !parsed.diagnostics.is_empty() {
		return recovery::recover(content, parsed, schema);
	}
	parsed
}

fn lex(
	content: &str,
	lua_mode: bool,
	close_string_at_line_end: Option<usize>,
) -> (Vec<Token>, Vec<ParseDiagnostic>) {
	let mut lexer = Lexer::new(content, lua_mode);
	lexer.close_string_at_line_end = close_string_at_line_end;
	let mut tokens = Vec::new();
	loop {
		let token = lexer.next_token();
		let is_eof = matches!(token.kind, TokenKind::Eof);
		tokens.push(token);
		if is_eof {
			break;
		}
	}
	(tokens, lexer.take_diagnostics())
}

#[cfg(test)]
mod tests {
	use super::{
		AstStatement, AstValue, ParseDiagnosticCode, ScalarValue, ScriptSyntax,
		parse_clausewitz_content, parse_clausewitz_file, parse_clausewitz_statements,
	};
	use crate::model::GamePath;
	use std::fs;
	use std::path::Path;

	fn game_path(text: &str) -> &GamePath {
		GamePath::new(text).expect("valid game path")
	}

	fn assignment_keys(statements: &[AstStatement]) -> Vec<&str> {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.as_str()),
				_ => None,
			})
			.collect()
	}

	#[test]
	fn the_ast_carries_the_game_path_it_was_parsed_at() {
		let path = game_path("common/scripted_effects/a.txt");
		let parsed = parse_clausewitz_content(path, "a = { }\n");
		assert_eq!(parsed.ast.path.as_game_path(), path);
		assert_eq!(assignment_keys(&parsed.ast.statements), vec!["a"]);
	}

	#[test]
	fn a_lua_game_path_is_parsed_with_lua_comments_in_any_case() {
		let has_comment = |statements: &[AstStatement]| {
			statements
				.iter()
				.any(|statement| matches!(statement, AstStatement::Comment { .. }))
		};
		let source = "-- comment\nx = 1\n";
		for text in [
			"common/defines/00_defines.lua",
			"common/defines/00_DEFINES.LUA",
		] {
			let parsed = parse_clausewitz_content(game_path(text), source);
			assert!(
				parsed.diagnostics.is_empty(),
				"{text}: {:?}",
				parsed.diagnostics
			);
			assert!(has_comment(&parsed.ast.statements), "{text}");
			assert_eq!(assignment_keys(&parsed.ast.statements), vec!["x"], "{text}");
		}
		let script = parse_clausewitz_content(game_path("common/defines/00_defines.txt"), source);
		assert!(
			!has_comment(&script.ast.statements),
			"`--` is not a comment outside Lua"
		);
	}

	#[test]
	fn syntax_follows_the_extension_of_either_kind_of_path() {
		assert_eq!(
			ScriptSyntax::for_game_path(game_path("map/random/tweaks.lua")),
			ScriptSyntax::Lua
		);
		assert_eq!(
			ScriptSyntax::for_game_path(game_path("interface/a.gui")),
			ScriptSyntax::Clausewitz
		);
		assert_eq!(
			ScriptSyntax::for_physical_path(Path::new("/tmp/Defines.Lua")),
			ScriptSyntax::Lua
		);
		assert_eq!(
			ScriptSyntax::for_physical_path(Path::new("/tmp/no_extension")),
			ScriptSyntax::Clausewitz
		);
	}

	#[test]
	fn statements_are_parsed_without_any_path() {
		let parsed = parse_clausewitz_statements(ScriptSyntax::Lua, "-- c\nx = 1\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(assignment_keys(&parsed.statements), vec!["x"]);
	}

	#[test]
	fn a_physical_file_is_parsed_without_a_game_path() {
		let temp = tempfile::tempdir().expect("temp dir");
		let file = temp.path().join("tweaks.lua");
		fs::write(&file, "-- c\nx = 1\n").expect("write script");
		let parsed = parse_clausewitz_file(&file);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(assignment_keys(&parsed.statements), vec!["x"]);

		let missing = parse_clausewitz_file(&temp.path().join("missing.txt"));
		assert!(missing.statements.is_empty());
		assert!(
			missing.diagnostics[0]
				.message
				.starts_with("failed to read file")
		);
	}

	#[test]
	fn unclosed_block_reports_eof_and_retains_parsed_content() {
		let source = "g = { old = { primary = AAA }";
		let parsed = parse_clausewitz_content(game_path("common/cultures/test.txt"), source);
		assert_eq!(parsed.diagnostics.len(), 1);
		assert_eq!(
			parsed.diagnostics[0].code,
			ParseDiagnosticCode::MissingClosingBrace
		);
		assert_eq!(parsed.diagnostics[0].span.start.offset, source.len());
		assert_eq!(parsed.ast.statements.len(), 1);
	}

	#[test]
	fn extra_closing_brace_is_reported_without_discarding_following_groups() {
		let source =
			include_str!("../../../../tests/fixtures/cultures/malformed/extra_closing_brace.txt");
		let parsed = parse_clausewitz_content(game_path("common/cultures/test.txt"), source);
		assert_eq!(parsed.diagnostics.len(), 1);
		assert_eq!(
			parsed.diagnostics[0].code,
			ParseDiagnosticCode::UnmatchedClosingBrace
		);
		assert_eq!(parsed.diagnostics[0].span.start.line, 5);
		assert_eq!(
			parsed
				.ast
				.statements
				.iter()
				.filter(|statement| matches!(statement, AstStatement::Assignment { .. }))
				.count(),
			2
		);
	}

	#[test]
	fn parser_handles_assignments_and_lists() {
		let parsed = parse_clausewitz_content(
			game_path("test.txt"),
			"name = \"x\"\ntags = {\n\t\"A\"\n\t\"B\"\n}\n",
		);
		assert!(parsed.diagnostics.is_empty());
		assert_eq!(parsed.ast.statements.len(), 2);

		let AstStatement::Assignment { value, .. } = &parsed.ast.statements[1] else {
			panic!("expected assignment");
		};

		let AstValue::Block { items, .. } = value else {
			panic!("expected block value");
		};

		assert_eq!(items.len(), 2);
	}

	#[test]
	fn parser_treats_digit_leading_identifier_as_assignment_key() {
		let parsed = parse_clausewitz_content(
			game_path("common/powerprojection/00_static.txt"),
			"25_permanent_power_projection = { yearly_decay = 1 }\n",
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 1, "{:#?}", parsed.ast);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected digit-leading assignment");
		};
		assert_eq!(key, "25_permanent_power_projection");
		let AstValue::Block { items, .. } = value else {
			panic!("expected definition block");
		};
		let AstStatement::Assignment { value, .. } = &items[0] else {
			panic!("expected numeric field assignment");
		};
		assert!(matches!(
			value,
			AstValue::Scalar {
				value: ScalarValue::Number(number),
				..
			} if number == "1"
		));
	}

	#[test]
	fn parser_accepts_nested_equals_assignment_forms() {
		let parsed = parse_clausewitz_content(
			game_path("missions.txt"),
			"custom_tooltip = njd_unite_arabia_tooltip = { factor = 1 }\ncenter_of_trade = 1 = yes\n286 = = { owner = ROOT }\n",
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		for statement in &parsed.ast.statements {
			let AstStatement::Assignment { value, .. } = statement else {
				panic!("expected assignment");
			};
			match value {
				AstValue::Block { .. } | AstValue::Scalar { .. } => {}
			}
		}
	}

	#[test]
	fn parser_accepts_implicit_block_assignments_without_equals() {
		let parsed = parse_clausewitz_content(
			game_path("scripted_effects.txt"),
			"some_effect {\n\t$who$ = {\n\t\ttrigger_switch = { 100 = { PREV = { add_prestige = 1 } } }\n\t}\n}\n",
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 1);
		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected implicit block assignment");
		};
		assert_eq!(key, "some_effect");
		let AstValue::Block { items, .. } = value else {
			panic!("expected block value");
		};
		assert!(!items.is_empty());
	}

	/// Only the exact lowercase spelling is boolean.
	///
	/// The game decides a boolean in `CToken::GetBool`, a case-sensitive
	/// `strncmp` against `yes`, so `YES` is not true to it. foch used to lower
	/// the word before matching and emit the canonical `yes`, which rewrote a
	/// value the game reads as false into one it reads as true.
	#[test]
	fn only_lowercase_yes_and_no_are_boolean() {
		for (source, expected) in [
			("v = yes\n", ScalarValue::Bool(true)),
			("v = no\n", ScalarValue::Bool(false)),
			("v = YES\n", ScalarValue::Identifier("YES".to_string())),
			("v = Yes\n", ScalarValue::Identifier("Yes".to_string())),
			("v = NO\n", ScalarValue::Identifier("NO".to_string())),
		] {
			let parsed = parse_clausewitz_content(game_path("test.txt"), source);
			assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
			let [AstStatement::Assignment { value, .. }] = parsed.ast.statements.as_slice() else {
				panic!("expected one assignment for {source:?}")
			};
			let AstValue::Scalar { value, .. } = value else {
				panic!("expected a scalar for {source:?}")
			};
			assert_eq!(value, &expected, "{source:?}");
		}
	}

	/// The round trip must not change which value the game reads.
	#[test]
	fn emitting_a_non_lowercase_yes_keeps_its_spelling() {
		let parsed = parse_clausewitz_content(game_path("test.txt"), "v = YES\n");
		let rendered =
			crate::game::eu4::script::emit::emit_clausewitz_statements(&parsed.ast.statements)
				.expect("emit");

		assert!(rendered.contains("v = YES"), "{rendered}");
	}

	#[test]
	fn parser_keeps_escaped_quotes_inside_multiline_strings() {
		let parsed = parse_clausewitz_content(
			game_path("scripted_effects.txt"),
			r#"event_wrapper = {
	effect = "
		if = {
			limit = { has_dlc = \"Mandate of Heaven\" }
		}
	"
}
next_effect = { add_prestige = 1 }
"#,
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 2, "{:#?}", parsed.ast);
		assert!(
			parsed
				.ast
				.statements
				.iter()
				.all(|statement| matches!(statement, AstStatement::Assignment { .. }))
		);

		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &parsed.ast.statements[0]
		else {
			panic!("expected event wrapper block");
		};
		let AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} = &items[0]
		else {
			panic!("expected multiline effect string");
		};
		assert_eq!(key, "effect");
		assert!(value.as_text().contains(r#"\"Mandate of Heaven\""#));
	}

	#[test]
	fn lua_mode_recognizes_double_dash_line_comment() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "-- foo\nbar=1\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 2);

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[0] else {
			panic!("expected comment");
		};
		assert!(text.contains("foo"));

		let AstStatement::Assignment { key, .. } = &parsed.ast.statements[1] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "bar");
	}

	#[test]
	fn lua_mode_recognizes_inline_comment_after_value() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "x = 1 -- trail\ny = 2\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("1".to_string()));

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[1] else {
			panic!("expected comment");
		};
		assert!(text.contains("trail"));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[2] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "y");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("2".to_string()));
	}

	#[test]
	fn lua_mode_recognizes_inline_comment_after_identifier_no_space() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "x = yes--c\ny = 2\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Bool(true));

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[1] else {
			panic!("expected comment");
		};
		assert!(text.contains("c"));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[2] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "y");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("2".to_string()));
	}

	#[test]
	fn lua_mode_recognizes_inline_comment_after_number_no_space() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "x = 60--c\ny = 2\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("60".to_string()));

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[1] else {
			panic!("expected comment");
		};
		assert!(text.contains("c"));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[2] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "y");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("2".to_string()));
	}

	#[test]
	fn lua_mode_recognizes_inline_comment_after_string_no_space() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "x = \"a\"--c\ny = 2\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::String("a".to_string()));

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[1] else {
			panic!("expected comment");
		};
		assert!(text.contains("c"));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[2] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "y");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("2".to_string()));
	}

	#[test]
	fn lua_mode_negative_number_still_works() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "a = -1\nb = -0.5\nc = -\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 3);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "a");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("-1".to_string()));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[1] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "b");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("-0.5".to_string()));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[2] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "c");
		let AstValue::Scalar { .. } = value else {
			panic!("expected scalar value");
		};
	}

	#[test]
	fn lua_mode_comma_separates_numeric_scalars_and_preserves_exponents() {
		let parsed = parse_clausewitz_content(
			game_path("defines.lua"),
			"plain = 75,\npositive_exp = 1e-5,\nnegative_exp = -1e-5,\nstandalone = -,\n",
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 4, "{:#?}", parsed.ast);

		for (statement, (expected_key, expected_number)) in parsed.ast.statements.iter().zip([
			("plain", "75"),
			("positive_exp", "1e-5"),
			("negative_exp", "-1e-5"),
			("standalone", "-"),
		]) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				panic!("expected assignment");
			};
			assert_eq!(key, expected_key);
			assert!(matches!(
				value,
				AstValue::Scalar {
					value: ScalarValue::Number(number),
					..
				} if number == expected_number
			));
		}
	}

	#[test]
	fn lua_mode_block_comment_level_zero() {
		let parsed = parse_clausewitz_content(
			game_path("test.lua"),
			"--[[ first line\nsecond line ]]\nx = 1\n",
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 2);

		let AstStatement::Comment { text, span } = &parsed.ast.statements[0] else {
			panic!("expected comment");
		};
		assert!(text.contains("first line"));
		assert!(text.contains("second line"));
		assert_eq!(span.end.line, 2);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[1] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("1".to_string()));
	}

	#[test]
	fn lua_mode_block_comment_level_two() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "--[==[ a ]==]\nx = 1\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 2);

		let AstStatement::Comment { text, .. } = &parsed.ast.statements[0] else {
			panic!("expected comment");
		};
		assert!(text.contains("a"));

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[1] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "x");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("1".to_string()));
	}

	#[test]
	fn lua_mode_unterminated_block_comment_emits_diagnostic() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "--[[ no end\n");
		assert!(!parsed.diagnostics.is_empty());
		assert!(parsed.diagnostics.iter().any(|diagnostic| {
			diagnostic
				.message
				.to_ascii_lowercase()
				.contains("unterminated")
		}));
		assert!(
			parsed
				.ast
				.statements
				.iter()
				.any(|statement| matches!(statement, AstStatement::Comment { .. }))
		);
	}

	#[test]
	fn lua_mode_dotted_keys_normalize_to_assignment() {
		let parsed = parse_clausewitz_content(game_path("test.lua"), "NDefines.NCountry.X = 0.5\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert_eq!(parsed.ast.statements.len(), 1);

		let AstStatement::Assignment { key, value, .. } = &parsed.ast.statements[0] else {
			panic!("expected assignment");
		};
		assert_eq!(key, "NDefines.NCountry.X");
		let AstValue::Scalar { value, .. } = value else {
			panic!("expected scalar value");
		};
		assert_eq!(value, &ScalarValue::Number("0.5".to_string()));
	}

	#[test]
	fn non_lua_mode_treats_double_dash_as_numbers() {
		let parsed = parse_clausewitz_content(game_path("test.txt"), "-- foo\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert!(parsed.ast.statements.len() >= 2);
		assert!(
			parsed
				.ast
				.statements
				.iter()
				.all(|statement| !matches!(statement, AstStatement::Comment { .. }))
		);
		assert!(parsed.ast.statements.iter().any(|statement| matches!(
			statement,
			AstStatement::Item {
				value: AstValue::Scalar {
					value: ScalarValue::Number(number),
					..
				},
				..
			} if number.as_str() == "-"
		)));
	}

	#[test]
	fn lua_mode_off_for_unknown_extension() {
		let parsed = parse_clausewitz_content(game_path("test.gui"), "-- foo\n");
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		assert!(parsed.ast.statements.len() >= 2);
		assert!(
			parsed
				.ast
				.statements
				.iter()
				.all(|statement| !matches!(statement, AstStatement::Comment { .. }))
		);
		assert!(parsed.ast.statements.iter().any(|statement| matches!(
			statement,
			AstStatement::Item {
				value: AstValue::Scalar {
					value: ScalarValue::Number(number),
					..
				},
				..
			} if number.as_str() == "-"
		)));
	}
}
