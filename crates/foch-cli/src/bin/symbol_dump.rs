use foch_core::model::{SemanticIndex, SymbolDefinition, SymbolKind};
use foch_language::analyzer::semantic_index::{build_semantic_index, parse_script_file};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
struct SymbolLocation {
	path: String,
	line: usize,
	column: usize,
	mod_id: String,
	module: String,
}

#[derive(Debug, Serialize)]
struct SymbolEntry {
	kind: String,
	name: String,
	definition_count: usize,
	modules: Vec<String>,
	locations: Vec<SymbolLocation>,
}

#[derive(Debug, Serialize)]
struct SymbolDumpMeta {
	root: String,
	parsed_files: usize,
	scopes: usize,
	definitions: usize,
	references: usize,
	alias_usages: usize,
}

#[derive(Debug, Serialize)]
struct SymbolDump {
	meta: SymbolDumpMeta,
	symbols: Vec<SymbolEntry>,
}

fn main() {
	let mut args = std::env::args().skip(1);
	let Some(root_arg) = args.next() else {
		eprintln!("usage: cargo run --bin symbol_dump -- <eu4_root> [output_json]");
		std::process::exit(1);
	};
	let output = args
		.next()
		.unwrap_or_else(|| "/tmp/foch-symbol-table.json".to_string());

	let root = PathBuf::from(root_arg);
	if !root.is_dir() {
		eprintln!("invalid root: {}", root.display());
		std::process::exit(1);
	}

	let files = collect_semantic_script_files(&root);
	let mod_id = "__game__eu4";
	let mut parsed = Vec::with_capacity(files.len());
	for file in files {
		if let Some(item) = parse_script_file(mod_id, &root, &file) {
			parsed.push(item);
		}
	}

	let index = build_semantic_index(&parsed);
	let symbols = build_symbol_entries(&index);
	let dump = SymbolDump {
		meta: SymbolDumpMeta {
			root: root.display().to_string(),
			parsed_files: parsed.len(),
			scopes: index.scopes.len(),
			definitions: index.definitions.len(),
			references: index.references.len(),
			alias_usages: index.alias_usages.len(),
		},
		symbols,
	};

	match serde_json::to_string_pretty(&dump) {
		Ok(raw) => {
			if let Err(err) = std::fs::write(&output, raw) {
				eprintln!("write output failed: {err}");
				std::process::exit(1);
			}
			println!("output={output}");
			println!("parsed_files={}", dump.meta.parsed_files);
			println!("definitions={}", dump.meta.definitions);
			println!("unique_symbols={}", dump.symbols.len());
		}
		Err(err) => {
			eprintln!("serialize failed: {err}");
			std::process::exit(1);
		}
	}
}

fn collect_semantic_script_files(root: &Path) -> Vec<PathBuf> {
	let mut files = Vec::new();
	let targets = [
		"events",
		"decisions",
		"common/scripted_effects",
		"common/diplomatic_actions",
		"common/triggered_modifiers",
		"common/defines",
	];
	for target in targets {
		let dir = root.join(target);
		if !dir.is_dir() {
			continue;
		}
		for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
			if !entry.file_type().is_file() {
				continue;
			}
			let path = entry.path();
			let Some(ext) = path.extension() else {
				continue;
			};
			let ext = ext.to_string_lossy();
			if matches!(ext.to_ascii_lowercase().as_str(), "txt" | "lua") {
				files.push(path.to_path_buf());
			}
		}
	}
	files.sort();
	files.dedup();
	files
}

fn build_symbol_entries(index: &SemanticIndex) -> Vec<SymbolEntry> {
	let mut grouped: BTreeMap<(String, String), Vec<&SymbolDefinition>> = BTreeMap::new();
	for def in &index.definitions {
		let key = (symbol_kind_name(def.kind).to_string(), def.name.clone());
		grouped.entry(key).or_default().push(def);
	}

	let mut entries = Vec::new();
	for ((kind, name), defs) in grouped {
		let mut modules: Vec<String> = defs.iter().map(|d| d.module.clone()).collect();
		modules.sort();
		modules.dedup();

		let mut locations: Vec<SymbolLocation> = defs
			.iter()
			.map(|d| SymbolLocation {
				path: d.path.display().to_string(),
				line: d.line,
				column: d.column,
				mod_id: d.mod_id.clone(),
				module: d.module.clone(),
			})
			.collect();
		locations.sort_by(|a, b| {
			a.path
				.cmp(&b.path)
				.then(a.line.cmp(&b.line))
				.then(a.column.cmp(&b.column))
		});
		locations.truncate(8);

		entries.push(SymbolEntry {
			kind,
			name,
			definition_count: defs.len(),
			modules,
			locations,
		});
	}

	entries
}

fn symbol_kind_name(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "ScriptedEffect",
		SymbolKind::ScriptedTrigger => "ScriptedTrigger",
		SymbolKind::Event => "Event",
		SymbolKind::Decision => "Decision",
		SymbolKind::DiplomaticAction => "DiplomaticAction",
		SymbolKind::TriggeredModifier => "TriggeredModifier",
	}
}
