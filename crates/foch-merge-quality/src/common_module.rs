use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use foch_core::domain::descriptor::load_descriptor;
use foch_engine::canonicalize_clausewitz_file;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, parse_clausewitz_file};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommonModuleDiagnostic {
	pub phase: String,
	pub path: Option<String>,
	pub message: String,
}

#[derive(Clone, Debug)]
struct ParsedModuleFile {
	statements: Vec<AstStatement>,
	diagnostics: Vec<CommonModuleDiagnostic>,
}

#[derive(Clone, Debug)]
struct ParsedModuleLayer {
	replace_namespace: bool,
	files: BTreeMap<String, Arc<ParsedModuleFile>>,
}

#[derive(Default)]
pub struct CommonModuleViewBuilder {
	layers: BTreeMap<(PathBuf, String), Arc<ParsedModuleLayer>>,
	views: BTreeMap<(Vec<PathBuf>, String), Arc<AstFile>>,
}

#[derive(Clone, Debug)]
pub struct NormalizedModuleComparison {
	pub candidate: AstFile,
	pub human: AstFile,
	pub diagnostics: Vec<CommonModuleDiagnostic>,
	pub reused_definitions: usize,
	pub normalized_definitions: usize,
}

pub fn normalize_module_comparison(
	candidate: &AstFile,
	human: &AstFile,
	policies: &MergePolicies,
	module_prefix: &str,
) -> NormalizedModuleComparison {
	if [candidate, human].iter().any(|file| {
		file.statements
			.iter()
			.any(|statement| matches!(statement, AstStatement::Item { .. }))
	}) {
		let mut diagnostics = Vec::new();
		let candidate = canonicalize_or_retain(
			candidate,
			policies,
			"candidate",
			"whole module",
			&mut diagnostics,
		);
		let human =
			canonicalize_or_retain(human, policies, "human", "whole module", &mut diagnostics);
		return NormalizedModuleComparison {
			candidate,
			human,
			diagnostics,
			reused_definitions: 0,
			normalized_definitions: 1,
		};
	}

	let keys = top_level_assignment_keys(candidate)
		.into_iter()
		.chain(top_level_assignment_keys(human))
		.collect::<BTreeSet<_>>();
	let differing = keys
		.iter()
		.filter(|key| {
			let candidate = select_definition(candidate, key);
			let human = select_definition(human, key);
			!statements_content_equal(&candidate.statements, &human.statements)
		})
		.count();
	let progress_step = (differing / 10).max(1);
	let started = Instant::now();
	let mut candidate_statements = Vec::new();
	let mut human_statements = Vec::new();
	let mut diagnostics = Vec::new();
	let mut reused_definitions = 0;
	let mut normalized_definitions = 0;
	for key in keys {
		let candidate_group = select_definition(candidate, &key);
		let human_group = select_definition(human, &key);
		if statements_content_equal(&candidate_group.statements, &human_group.statements) {
			candidate_statements.extend(candidate_group.statements);
			human_statements.extend(human_group.statements);
			reused_definitions += 1;
			continue;
		}

		let scope = format!("definition `{key}`");
		let normalized_candidate = canonicalize_or_retain(
			&candidate_group,
			policies,
			"candidate",
			&scope,
			&mut diagnostics,
		);
		let normalized_human =
			canonicalize_or_retain(&human_group, policies, "human", &scope, &mut diagnostics);
		candidate_statements.extend(normalized_candidate.statements);
		human_statements.extend(normalized_human.statements);
		normalized_definitions += 1;
		if normalized_definitions == differing
			|| normalized_definitions == 1
			|| normalized_definitions % progress_step == 0
		{
			let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
			let eta_ms = elapsed_ms.saturating_mul((differing - normalized_definitions) as u64)
				/ normalized_definitions as u64;
			eprintln!(
				"[common-module] {module_prefix} comparison {normalized_definitions}/{differing} reused={reused_definitions} elapsed_ms={elapsed_ms} eta_ms={eta_ms}"
			);
		}
	}
	candidate_statements.sort_by(compare_top_level_statements);
	human_statements.sort_by(compare_top_level_statements);
	NormalizedModuleComparison {
		candidate: AstFile {
			path: candidate.path.clone(),
			statements: candidate_statements,
		},
		human: AstFile {
			path: human.path.clone(),
			statements: human_statements,
		},
		diagnostics,
		reused_definitions,
		normalized_definitions,
	}
}

fn canonicalize_or_retain(
	file: &AstFile,
	policies: &MergePolicies,
	side: &str,
	scope: &str,
	diagnostics: &mut Vec<CommonModuleDiagnostic>,
) -> AstFile {
	match canonicalize_clausewitz_file(file, policies) {
		Ok(canonical) => canonical,
		Err(error) => {
			diagnostics.push(CommonModuleDiagnostic {
				phase: "comparison_normalization".to_string(),
				path: Some(scope.to_string()),
				message: format!("{side}: {error}"),
			});
			file.clone()
		}
	}
}

fn select_definition(ast: &AstFile, key: &str) -> AstFile {
	AstFile {
		path: ast.path.clone(),
		statements: ast
			.statements
			.iter()
			.filter(|statement| {
				matches!(statement, AstStatement::Assignment { key: candidate, .. } if candidate == key)
			})
			.cloned()
			.collect(),
	}
}

fn top_level_assignment_keys(ast: &AstFile) -> BTreeSet<String> {
	ast.statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.clone()),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		})
		.collect()
}

fn statements_content_equal(left: &[AstStatement], right: &[AstStatement]) -> bool {
	left.len() == right.len()
		&& left
			.iter()
			.zip(right)
			.all(|(left, right)| statement_content_equal(left, right))
}

fn statement_content_equal(left: &AstStatement, right: &AstStatement) -> bool {
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
		) => left_key == right_key && value_content_equal(left_value, right_value),
		(
			AstStatement::Item {
				value: left_value, ..
			},
			AstStatement::Item {
				value: right_value, ..
			},
		) => value_content_equal(left_value, right_value),
		(AstStatement::Comment { text: left, .. }, AstStatement::Comment { text: right, .. }) => {
			left == right
		}
		_ => false,
	}
}

fn value_content_equal(left: &AstValue, right: &AstValue) -> bool {
	match (left, right) {
		(AstValue::Scalar { value: left, .. }, AstValue::Scalar { value: right, .. }) => {
			left == right
		}
		(AstValue::Block { items: left, .. }, AstValue::Block { items: right, .. }) => {
			statements_content_equal(left, right)
		}
		_ => false,
	}
}

fn compare_top_level_statements(left: &AstStatement, right: &AstStatement) -> std::cmp::Ordering {
	match (left, right) {
		(
			AstStatement::Assignment { key: left, .. },
			AstStatement::Assignment { key: right, .. },
		) => left.cmp(right),
		(AstStatement::Assignment { .. }, _) => std::cmp::Ordering::Less,
		(_, AstStatement::Assignment { .. }) => std::cmp::Ordering::Greater,
		(AstStatement::Item { .. }, AstStatement::Comment { .. }) => std::cmp::Ordering::Less,
		(AstStatement::Comment { .. }, AstStatement::Item { .. }) => std::cmp::Ordering::Greater,
		_ => std::cmp::Ordering::Equal,
	}
}

impl CommonModuleViewBuilder {
	pub fn view(
		&mut self,
		roots: &[&Path],
		module_prefix: &str,
	) -> Result<Arc<AstFile>, Vec<CommonModuleDiagnostic>> {
		let key = (
			roots.iter().map(|root| root.to_path_buf()).collect(),
			module_prefix.to_string(),
		);
		if let Some(view) = self.views.get(&key) {
			return Ok(Arc::clone(view));
		}

		let mut visible = BTreeMap::<String, Arc<ParsedModuleFile>>::new();
		for root in roots {
			let layer = self.layer(root, module_prefix)?;
			if layer.replace_namespace {
				visible.clear();
			}
			for (relative, file) in &layer.files {
				visible.insert(relative.clone(), Arc::clone(file));
			}
		}

		let diagnostics = visible
			.values()
			.flat_map(|file| file.diagnostics.iter().cloned())
			.collect::<Vec<_>>();
		if !diagnostics.is_empty() {
			return Err(diagnostics);
		}

		let mut definitions = BTreeMap::<String, AstStatement>::new();
		let mut items = Vec::new();
		for file in visible.values() {
			for statement in &file.statements {
				match statement {
					AstStatement::Assignment { key, .. } => {
						definitions.insert(key.clone(), statement.clone());
					}
					AstStatement::Item { .. } => items.push(statement.clone()),
					AstStatement::Comment { .. } => {}
				}
			}
		}
		let mut statements = definitions.into_values().collect::<Vec<_>>();
		statements.extend(items);
		let view = Arc::new(AstFile {
			path: PathBuf::from(format!("{module_prefix}/__foch_common_module__.txt")),
			statements,
		});
		self.views.insert(key, Arc::clone(&view));
		Ok(view)
	}

	fn layer(
		&mut self,
		root: &Path,
		module_prefix: &str,
	) -> Result<Arc<ParsedModuleLayer>, Vec<CommonModuleDiagnostic>> {
		let key = (root.to_path_buf(), module_prefix.to_string());
		if let Some(layer) = self.layers.get(&key) {
			return Ok(Arc::clone(layer));
		}
		let layer = Arc::new(parse_module_layer(root, module_prefix)?);
		self.layers.insert(key, Arc::clone(&layer));
		Ok(layer)
	}
}

fn parse_module_layer(
	root: &Path,
	module_prefix: &str,
) -> Result<ParsedModuleLayer, Vec<CommonModuleDiagnostic>> {
	let replace_namespace = match layer_replaces_module(root, module_prefix) {
		Ok(replace) => replace,
		Err(diagnostic) => return Err(vec![diagnostic]),
	};
	let directory = root.join(module_prefix);
	if !directory.exists() {
		return Ok(ParsedModuleLayer {
			replace_namespace,
			files: BTreeMap::new(),
		});
	}
	if !directory.is_dir() {
		return Err(vec![CommonModuleDiagnostic {
			phase: "module_input".to_string(),
			path: Some(directory.display().to_string()),
			message: "module prefix is not a directory".to_string(),
		}]);
	}

	let mut files = BTreeMap::new();
	for entry in walkdir::WalkDir::new(&directory) {
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => {
				return Err(vec![CommonModuleDiagnostic {
					phase: "module_input".to_string(),
					path: error.path().map(|path| path.display().to_string()),
					message: error.to_string(),
				}]);
			}
		};
		if !entry.file_type().is_file()
			|| entry
				.path()
				.extension()
				.and_then(|extension| extension.to_str())
				.is_none_or(|extension| !extension.eq_ignore_ascii_case("txt"))
		{
			continue;
		}
		let path = entry.into_path();
		let relative = relative_path(root, &path);
		let parsed = parse_clausewitz_file(&path);
		let diagnostics = parsed
			.diagnostics
			.into_iter()
			.map(|diagnostic| CommonModuleDiagnostic {
				phase: "parse".to_string(),
				path: Some(relative.clone()),
				message: diagnostic.message,
			})
			.collect();
		files.insert(
			relative,
			Arc::new(ParsedModuleFile {
				statements: parsed.ast.statements,
				diagnostics,
			}),
		);
	}
	Ok(ParsedModuleLayer {
		replace_namespace,
		files,
	})
}

fn layer_replaces_module(root: &Path, module_prefix: &str) -> Result<bool, CommonModuleDiagnostic> {
	let descriptor_path = root.join("descriptor.mod");
	if !descriptor_path.is_file() {
		return Ok(false);
	}
	let descriptor = load_descriptor(&descriptor_path).map_err(|error| CommonModuleDiagnostic {
		phase: "descriptor".to_string(),
		path: Some(descriptor_path.display().to_string()),
		message: error.to_string(),
	})?;
	Ok(descriptor
		.replace_path
		.iter()
		.any(|replace_path| replace_path_covers_prefix(replace_path, module_prefix)))
}

fn replace_path_covers_prefix(replace_path: &str, module_prefix: &str) -> bool {
	let normalized = replace_path.trim().replace('\\', "/");
	let replace_path = normalized.trim_matches('/');
	let module_prefix = module_prefix.trim_matches('/');
	!replace_path.is_empty()
		&& (replace_path == module_prefix
			|| module_prefix
				.strip_prefix(replace_path)
				.is_some_and(|suffix| suffix.starts_with('/')))
}

fn relative_path(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	fn write_file(root: &Path, relative: &str, contents: &str) {
		let path = root.join(relative);
		fs::create_dir_all(path.parent().expect("fixture path has parent")).unwrap();
		fs::write(path, contents).unwrap();
	}

	fn assignment_keys(ast: &AstFile) -> Vec<String> {
		ast.statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
			})
			.collect()
	}

	#[test]
	fn merges_definitions_across_file_names() {
		let temp = tempfile::tempdir().unwrap();
		let base = temp.path().join("base");
		let overlay = temp.path().join("overlay");
		write_file(
			&base,
			"common/buildings/00_base.txt",
			"temple = { }\nmarket = { }\n",
		);
		write_file(
			&overlay,
			"common/buildings/99_mod.txt",
			"temple = { cost = 100 }\n",
		);

		let view = CommonModuleViewBuilder::default()
			.view(&[&base, &overlay], "common/buildings")
			.unwrap();
		assert_eq!(assignment_keys(&view), vec!["market", "temple"]);
	}

	#[test]
	fn covering_replace_path_clears_earlier_files() {
		let temp = tempfile::tempdir().unwrap();
		let base = temp.path().join("base");
		let replacement = temp.path().join("replacement");
		write_file(&base, "common/religions/base.txt", "christian = { }\n");
		write_file(
			&replacement,
			"descriptor.mod",
			"name = \"replacement\"\nreplace_path = \"common/religions\"\n",
		);
		write_file(
			&replacement,
			"common/religions/replacement.txt",
			"muslim = { }\n",
		);

		let view = CommonModuleViewBuilder::default()
			.view(&[&base, &replacement], "common/religions")
			.unwrap();
		assert_eq!(assignment_keys(&view), vec!["muslim"]);
	}
}
