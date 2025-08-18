use std::collections::VecDeque;
use tree_sitter::Language as TSLanguage;
use tree_sitter::Node as TSNode;
use tree_sitter::Parser as TSParser;
use tree_sitter::Tree as TSTree;
use tree_sitter_paradox;
use typed_builder::TypedBuilder;

pub trait ParadoxScriptParser {
	fn parse(&mut self, text: &str) -> Result<(), &str>;
	fn tree(&self) -> Option<&TSTree>;

	fn text(&self) -> Option<&str>;
	fn iter_nodes(&self) -> Result<(), &str>;
	fn find_nodes(&self, condition: fn(TSNode<'_>) -> bool) -> Result<Vec<TSNode<'_>>, &str>;
}

#[derive(TypedBuilder)]
pub struct TSParserWrapper {
	parser: TSParser,
	pub text: Option<String>,
	#[builder(default)]
	pub tree: Option<TSTree>,
}

impl TSParserWrapper {
	pub fn new() -> Box<dyn ParadoxScriptParser> {
		let mut parser = TSParser::new();
		let language = TSLanguage::from(tree_sitter_paradox::LANGUAGE);
		parser
			.set_language(&language)
			.expect("Error loading Paradox language");
		Box::new(TSParserWrapper { parser, tree: None, text: None })
	}
}

impl ParadoxScriptParser for TSParserWrapper {
	fn parse(&mut self, text: &str) -> Result<(), &str> {
		self.text = Some(text.to_string());
		self.tree = self.parser.parse(text, self.tree.as_ref());
		if self.tree.is_none() {
			return Err("No Tree is parsed");
		}
		Ok(())
	}

	fn tree(&self) -> Option<&TSTree> {
		self.tree.as_ref()
	}

	fn text(&self) -> Option<&str> {
		self.text.as_ref().map(|x| x.as_str())
	}

	fn iter_nodes(&self) -> Result<(), &str> {
		let tree = self.tree.as_ref().ok_or("No tree available. Please parse text first.")?;
		let text = self.text.as_ref().ok_or("No text available. Please parse text first.")?;
		let root = tree.root_node();

		let mut q = VecDeque::from([root]);

		while let Some(node) = q.pop_front() {                  // 队头取元素
			let s = node.utf8_text(text.as_ref()).unwrap_or("<invalid utf8>");
			println!("<{}>-<{}>", node.kind(), s);

			// 每个节点单独创建一个 cursor 更稳妥
			let mut c = node.walk();
			for child in node.children(&mut c) {
				q.push_back(child);                             // 子节点排到队尾
			}
		}
		Ok(())
	}

	fn find_nodes(&self, condition: fn(TSNode<'_>) -> bool) -> Result<Vec<TSNode<'_>>, &str> {
		match self.tree.as_ref() {
			Some(tree) => {
				let root = tree.root_node();
				let mut cursor = root.walk();
				let mut stack = vec![root];
				let mut results = Vec::new();

				while let Some(node) = stack.pop() {
					if condition(node) {
						results.push(node);
					}
					cursor = node.walk();
					for child in node.children(&mut cursor) {
						stack.push(child);
					}
				}
				Ok(results)
			}
			None => Err("No tree available. Please parse text first."),
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::get_corpus_path;
	use crate::parsing::{ParadoxScriptParser, TSParserWrapper};

	#[test]
	fn test_parse_descriptor() {
		let descriptor_path = get_corpus_path().join("defines").join("descriptor.mod");
		let descriptor_text = std::fs::read_to_string(descriptor_path).unwrap();
		let mut parser = TSParserWrapper::new();
		let tree = parser.parse(&descriptor_text);
		println!("{:#?}", tree);
		let all_nodes = parser.find_nodes(|node| {
			return true;
		});
		println!("{:#?}", all_nodes);
	}
}
