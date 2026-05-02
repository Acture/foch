use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AstFile {
	pub path: PathBuf,
	pub statements: Vec<AstStatement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseDiagnostic {
	pub message: String,
	pub span: SpanRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
	pub ast: AstFile,
	pub diagnostics: Vec<ParseDiagnostic>,
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
				self.advance_byte();
				let text_start = self.index;
				while let Some(next) = self.peek_byte() {
					if next == b'"' {
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].to_string();
				if self.peek_byte() == Some(b'"') {
					self.advance_byte();
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
				self.advance_byte();
				while let Some(next) = self.peek_byte() {
					if !next.is_ascii_digit() && next != b'.' {
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].to_string();
				Token {
					kind: TokenKind::Number(text),
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
					if is_token_delimiter(next) {
						break;
					}
					if self.lua_mode && next == b'-' && self.peek_byte_at(1) == Some(b'-') {
						break;
					}
					self.advance_byte();
				}
				let text = self.source[text_start..self.index].trim().to_string();
				let lower = text.to_ascii_lowercase();
				let kind = match lower.as_str() {
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
							message: format!(
								"unterminated Lua block comment --[{}[",
								"=".repeat(level)
							),
							span: SpanRange {
								start: start.clone(),
								end: self.current_span(),
							},
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

	fn parse_file(mut self, path: PathBuf) -> ParseResult {
		let statements = self.parse_statements(false);
		ParseResult {
			ast: AstFile { path, statements },
			diagnostics: self.diagnostics,
		}
	}

	fn parse_statements(&mut self, stop_at_rbrace: bool) -> Vec<AstStatement> {
		let mut statements = Vec::new();

		loop {
			let token = self.peek();
			match &token.kind {
				TokenKind::Eof => break,
				TokenKind::RBrace if stop_at_rbrace => {
					self.bump();
					break;
				}
				TokenKind::Newline => {
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
					message: "could not parse statement start token".to_string(),
					span: first.span,
				});
				None
			}
		}
	}

	fn parse_value(&mut self) -> AstValue {
		let mut token = self.bump();
		while matches!(
			token.kind,
			TokenKind::Newline | TokenKind::Comment(_) | TokenKind::Eq
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
					message: "value parse failed; downgraded to empty identifier".to_string(),
					span: token.span.clone(),
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

pub fn parse_clausewitz_file(path: &Path) -> ParseResult {
	match std::fs::read(path) {
		Ok(bytes) => {
			let content = foch_core::decode_paradox_bytes(&bytes);
			parse_clausewitz_content(path.to_path_buf(), &content)
		}
		Err(err) => ParseResult {
			ast: AstFile {
				path: path.to_path_buf(),
				statements: Vec::new(),
			},
			diagnostics: vec![ParseDiagnostic {
				message: format!("failed to read file: {err}"),
				span: SpanRange {
					start: Span {
						line: 1,
						column: 1,
						offset: 0,
					},
					end: Span {
						line: 1,
						column: 1,
						offset: 0,
					},
				},
			}],
		},
	}
}

pub fn parse_clausewitz_content(path: PathBuf, content: &str) -> ParseResult {
	let lua_mode = path
		.extension()
		.and_then(|ext| ext.to_str())
		.is_some_and(|ext| ext.eq_ignore_ascii_case("lua"));
	let mut lexer = Lexer::new(content, lua_mode);
	let mut tokens = Vec::new();
	loop {
		let token = lexer.next_token();
		let is_eof = matches!(token.kind, TokenKind::Eof);
		tokens.push(token);
		if is_eof {
			break;
		}
	}
	let lexer_diagnostics = lexer.take_diagnostics();

	let mut result = ParserState::new(tokens).parse_file(path);
	result.diagnostics.extend(lexer_diagnostics);
	result
}

#[cfg(test)]
mod tests {
	use super::{AstStatement, AstValue, ScalarValue, parse_clausewitz_content};
	use std::path::PathBuf;

	#[test]
	fn parser_handles_assignments_and_lists() {
		let parsed = parse_clausewitz_content(
			PathBuf::from("test.txt"),
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
	fn parser_accepts_nested_equals_assignment_forms() {
		let parsed = parse_clausewitz_content(
			PathBuf::from("missions.txt"),
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
			PathBuf::from("scripted_effects.txt"),
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

	#[test]
	fn lua_mode_recognizes_double_dash_line_comment() {
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "-- foo\nbar=1\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "x = 1 -- trail\ny = 2\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "x = yes--c\ny = 2\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "x = 60--c\ny = 2\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "x = \"a\"--c\ny = 2\n");
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
		let parsed =
			parse_clausewitz_content(PathBuf::from("test.lua"), "a = -1\nb = -0.5\nc = -\n");
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
	fn lua_mode_block_comment_level_zero() {
		let parsed = parse_clausewitz_content(
			PathBuf::from("test.lua"),
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "--[==[ a ]==]\nx = 1\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.lua"), "--[[ no end\n");
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
		let parsed =
			parse_clausewitz_content(PathBuf::from("test.lua"), "NDefines.NCountry.X = 0.5\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.txt"), "-- foo\n");
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
		let parsed = parse_clausewitz_content(PathBuf::from("test.gui"), "-- foo\n");
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
