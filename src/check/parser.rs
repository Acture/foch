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
}

impl<'a> Lexer<'a> {
	fn new(source: &'a str) -> Self {
		Self {
			source,
			bytes: source.as_bytes(),
			index: 0,
			line: 1,
			column: 1,
		}
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
					message: "无法解析的语句起始 token".to_string(),
					span: first.span,
				});
				None
			}
		}
	}

	fn parse_value(&mut self) -> AstValue {
		let mut token = self.bump();
		while matches!(token.kind, TokenKind::Newline | TokenKind::Comment(_)) {
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
			TokenKind::Identifier(value) => AstValue::Scalar {
				value: ScalarValue::Identifier(value),
				span: token.span,
			},
			TokenKind::String(value) => AstValue::Scalar {
				value: ScalarValue::String(value),
				span: token.span,
			},
			TokenKind::Number(value) => AstValue::Scalar {
				value: ScalarValue::Number(value),
				span: token.span,
			},
			TokenKind::Bool(value) => AstValue::Scalar {
				value: ScalarValue::Bool(value),
				span: token.span,
			},
			TokenKind::Comment(text) => AstValue::Scalar {
				value: ScalarValue::Identifier(text),
				span: token.span,
			},
			_ => {
				self.diagnostics.push(ParseDiagnostic {
					message: "值解析失败，已降级为空标识符".to_string(),
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
			let content = String::from_utf8_lossy(&bytes);
			parse_clausewitz_content(path.to_path_buf(), &content)
		}
		Err(err) => ParseResult {
			ast: AstFile {
				path: path.to_path_buf(),
				statements: Vec::new(),
			},
			diagnostics: vec![ParseDiagnostic {
				message: format!("读取文件失败: {err}"),
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
	let mut lexer = Lexer::new(content);
	let mut tokens = Vec::new();
	loop {
		let token = lexer.next_token();
		let is_eof = matches!(token.kind, TokenKind::Eof);
		tokens.push(token);
		if is_eof {
			break;
		}
	}

	ParserState::new(tokens).parse_file(path)
}

#[cfg(test)]
mod tests {
	use super::{AstStatement, AstValue, parse_clausewitz_content};
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
}
