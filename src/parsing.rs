use tree_sitter::Language as TSLanguage;
use tree_sitter::Node as TSNode;
use tree_sitter::Parser as TSParser;
use tree_sitter::Tree as TSTree;
use tree_sitter_paradox;
use typed_builder::TypedBuilder;

pub trait ParadoxScriptParser {
	fn parse_as_tree(&mut self, text: &str) -> Result<(), &str>;
	fn find_nodes(&self, condition: fn(TSNode<'_>) -> bool) -> Result<Vec<TSNode<'_>>, &str>;
}

#[derive(TypedBuilder)]
pub(crate) struct TSParserWrapper {
	parser: TSParser,
	#[builder(default)]
	tree: Option<TSTree>,
}

impl TSParserWrapper {
	pub fn new() -> Box<dyn ParadoxScriptParser> {
		let mut parser = TSParser::new();
		let language = TSLanguage::from(tree_sitter_paradox::LANGUAGE);
		parser
			.set_language(&language)
			.expect("Error loading Paradox language");
		Box::new(TSParserWrapper { parser, tree: None })
	}
}

impl ParadoxScriptParser for TSParserWrapper {
	fn parse_as_tree(&mut self, text: &str) -> Result<(), &str> {
		self.tree = self.parser.parse(text, self.tree.as_ref());
		if self.tree.is_none() {
			return Err("No Tree is parsed");
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
		let tree = parser.parse_as_tree(&descriptor_text);
		println!("{:#?}", tree);
		let all_nodes = parser.find_nodes(|node| {
			return true;
		});
		println!("{:#?}", all_nodes);
	}
}
