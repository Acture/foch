use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};
use foch_syntax::{ByteSpan, ParadoxNode, ParadoxScalar};

pub fn paradox_node_to_ast_statement(node: &ParadoxNode<'_>, source: &str) -> Vec<AstStatement> {
	let converter = SpanConverter::new(source);
	converter.node_to_statements(node)
}

struct SpanConverter<'source> {
	source: &'source str,
	line_starts: Vec<usize>,
}

impl<'source> SpanConverter<'source> {
	fn new(source: &'source str) -> Self {
		let mut line_starts = vec![0];
		for (index, byte) in source.as_bytes().iter().enumerate() {
			if *byte == b'\n' {
				line_starts.push(index + 1);
			}
		}
		Self {
			source,
			line_starts,
		}
	}

	fn node_to_statements(&self, node: &ParadoxNode<'_>) -> Vec<AstStatement> {
		match node {
			ParadoxNode::Assignment {
				key,
				key_span,
				value,
				span,
			} => {
				let value_span = self.assignment_value_span(*span, *key_span);
				if let Some((prefix, prefix_span, remainder, remainder_span)) =
					self.split_numeric_prefixed_identifier_key(key, *key_span)
				{
					vec![
						AstStatement::Item {
							value: AstValue::Scalar {
								value: ScalarValue::Number(prefix.to_string()),
								span: self.span_range(prefix_span),
							},
							span: self.span_range(prefix_span),
						},
						AstStatement::Assignment {
							key: remainder.to_string(),
							key_span: self.span_range(remainder_span),
							value: self.node_to_value(value, value_span),
							span: SpanRange {
								start: self.position(remainder_span.start),
								end: self.position(span.end),
							},
						},
					]
				} else {
					vec![AstStatement::Assignment {
						key: key.as_text().into_owned(),
						key_span: self.span_range(*key_span),
						value: self.node_to_value(value, value_span),
						span: self.span_range(*span),
					}]
				}
			}
			ParadoxNode::Item { value, span } => vec![AstStatement::Item {
				value: self.node_to_value(value, *span),
				span: self.span_range(*span),
			}],
			ParadoxNode::Comment { text, span, .. } => vec![AstStatement::Comment {
				text: (*text).to_string(),
				span: self.span_range(*span),
			}],
			ParadoxNode::Condition {
				keyword,
				body,
				span,
			}
			| ParadoxNode::Logical {
				keyword,
				body,
				span,
			}
			| ParadoxNode::Scope {
				keyword,
				body,
				span,
			} => vec![AstStatement::Assignment {
				key: (*keyword).to_string(),
				key_span: self.leading_key_span(*span, keyword.len()),
				value: self.node_to_value(body, body.span().unwrap_or(*span)),
				span: self.span_range(*span),
			}],
			ParadoxNode::MacroMap { key, items, span } => vec![AstStatement::Assignment {
				key: key.as_text().into_owned(),
				key_span: self.leading_key_span(
					ByteSpan {
						start: span.start + 2,
						end: span.end,
					},
					key.as_text().len(),
				),
				value: AstValue::Block {
					items: self.nodes_to_statements(items),
					span: self.span_range(*span),
				},
				span: self.span_range(*span),
			}],
			ParadoxNode::Block { span, .. }
			| ParadoxNode::Array { span, .. }
			| ParadoxNode::CwtMarker { span, .. } => vec![AstStatement::Item {
				value: self.node_to_value(node, *span),
				span: self.span_range(*span),
			}],
			ParadoxNode::Scalar(_) => vec![AstStatement::Item {
				value: self.node_to_value(node, ByteSpan { start: 0, end: 0 }),
				span: self.span_range(ByteSpan { start: 0, end: 0 }),
			}],
		}
	}

	fn nodes_to_statements(&self, nodes: &[ParadoxNode<'_>]) -> Vec<AstStatement> {
		nodes
			.iter()
			.flat_map(|node| self.node_to_statements(node))
			.collect()
	}

	fn node_to_value(&self, node: &ParadoxNode<'_>, fallback_span: ByteSpan) -> AstValue {
		match node {
			ParadoxNode::Block { items, span } | ParadoxNode::Array { items, span } => {
				AstValue::Block {
					items: self.nodes_to_statements(items),
					span: self.span_range(*span),
				}
			}
			ParadoxNode::Scalar(value) => AstValue::Scalar {
				value: scalar_to_legacy(value),
				span: self.span_range(fallback_span),
			},
			ParadoxNode::Item { value, span } => self.node_to_value(value, *span),
			ParadoxNode::Comment { text, span, .. } => AstValue::Scalar {
				value: ScalarValue::Identifier((*text).to_string()),
				span: self.span_range(*span),
			},
			ParadoxNode::CwtMarker { payload, span, .. } => AstValue::Scalar {
				value: ScalarValue::Identifier((*payload).to_string()),
				span: self.span_range(*span),
			},
			ParadoxNode::Condition {
				keyword,
				body,
				span,
			}
			| ParadoxNode::Logical {
				keyword,
				body,
				span,
			}
			| ParadoxNode::Scope {
				keyword,
				body,
				span,
			} => AstValue::Block {
				items: vec![AstStatement::Assignment {
					key: (*keyword).to_string(),
					key_span: self.leading_key_span(*span, keyword.len()),
					value: self.node_to_value(body, body.span().unwrap_or(*span)),
					span: self.span_range(*span),
				}],
				span: self.span_range(*span),
			},
			ParadoxNode::MacroMap { key, items, span } => AstValue::Block {
				items: vec![AstStatement::Assignment {
					key: key.as_text().into_owned(),
					key_span: self.leading_key_span(
						ByteSpan {
							start: span.start + 2,
							end: span.end,
						},
						key.as_text().len(),
					),
					value: AstValue::Block {
						items: self.nodes_to_statements(items),
						span: self.span_range(*span),
					},
					span: self.span_range(*span),
				}],
				span: self.span_range(*span),
			},
			ParadoxNode::Assignment { .. } => AstValue::Block {
				items: self.node_to_statements(node),
				span: self.span_range(fallback_span),
			},
		}
	}

	fn assignment_value_span(&self, span: ByteSpan, key_span: ByteSpan) -> ByteSpan {
		let Some(relative_eq) = self.source[span.start..span.end].find('=') else {
			return ByteSpan {
				start: key_span.end,
				end: span.end,
			};
		};
		let mut start = span.start + relative_eq + 1;
		let bytes = self.source.as_bytes();
		while start < span.end && matches!(bytes[start], b' ' | b'\t' | b'\r' | b'\n') {
			start += 1;
		}
		ByteSpan {
			start,
			end: span.end,
		}
	}

	fn split_numeric_prefixed_identifier_key(
		&self,
		key: &ParadoxScalar<'source>,
		key_span: ByteSpan,
	) -> Option<(&'source str, ByteSpan, &'source str, ByteSpan)> {
		let ParadoxScalar::Identifier(text) = key else {
			return None;
		};
		let split_index = numeric_prefix_len(text);
		if split_index == 0 || split_index >= text.len() {
			return None;
		}
		Some((
			&text[..split_index],
			ByteSpan {
				start: key_span.start,
				end: key_span.start + split_index,
			},
			&text[split_index..],
			ByteSpan {
				start: key_span.start + split_index,
				end: key_span.end,
			},
		))
	}

	fn leading_key_span(&self, span: ByteSpan, key_len: usize) -> SpanRange {
		self.span_range(ByteSpan {
			start: span.start,
			end: span.start + key_len,
		})
	}

	fn span_range(&self, span: ByteSpan) -> SpanRange {
		SpanRange {
			start: self.position(span.start),
			end: self.position(span.end),
		}
	}

	fn position(&self, offset: usize) -> Span {
		let offset = offset.min(self.source.len());
		let index = self
			.line_starts
			.partition_point(|line_start| *line_start <= offset)
			.saturating_sub(1);
		let line_start = self.line_starts[index];
		Span {
			line: index + 1,
			column: offset - line_start + 1,
			offset,
		}
	}
}

fn scalar_to_legacy(value: &ParadoxScalar<'_>) -> ScalarValue {
	match value {
		ParadoxScalar::Identifier(text) => ScalarValue::Identifier((*text).to_string()),
		ParadoxScalar::String(text) => ScalarValue::String((*text).to_string()),
		ParadoxScalar::Number(text) => ScalarValue::Number((*text).to_string()),
		ParadoxScalar::Bool(value) => ScalarValue::Bool(*value),
		ParadoxScalar::Variable(text) => ScalarValue::Identifier(format!("${text}$")),
		ParadoxScalar::Template(text) => ScalarValue::Identifier((*text).to_string()),
	}
}

fn numeric_prefix_len(text: &str) -> usize {
	let bytes = text.as_bytes();
	let mut index = 0;
	if bytes.first() == Some(&b'-') {
		index += 1;
	}
	while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
		index += 1;
	}
	index
}
