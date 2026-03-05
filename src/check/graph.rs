use crate::check::model::{GraphFormat, ScopeKind, ScopeType, SemanticIndex, SymbolKind};
use std::collections::HashMap;

pub fn export_graph(index: &SemanticIndex, format: GraphFormat) -> String {
	match format {
		GraphFormat::Json => serde_json::to_string_pretty(index).unwrap_or_else(|err| {
			format!(
				"{{\"error\":\"graph json export failed: {}\"}}",
				escape_json(err.to_string())
			)
		}),
		GraphFormat::Dot => export_dot(index),
	}
}

fn export_dot(index: &SemanticIndex) -> String {
	let mut lines = Vec::new();
	lines.push("digraph foch_semantic {".to_string());
	lines.push("\trankdir=LR;".to_string());
	lines.push("\tnode [fontname=\"monospace\"];".to_string());

	for scope in &index.scopes {
		let label = format!(
			"scope:{}\\n{}\\nTHIS={}\\n{}:{}",
			scope.id,
			scope_kind_text(scope.kind),
			scope_type_text(scope.this_type),
			scope.path.display(),
			scope.span.line
		);
		lines.push(format!(
			"\tscope_{} [shape=oval,label=\"{}\"];",
			scope.id,
			escape_dot(&label)
		));
		if let Some(parent) = scope.parent {
			lines.push(format!("\tscope_{} -> scope_{};", parent, scope.id));
		}
	}

	for (idx, def) in index.definitions.iter().enumerate() {
		let label = format!(
			"def:{}\\n{}\\n{}\\n{}:{}",
			idx,
			symbol_kind_text(def.kind),
			def.name,
			def.path.display(),
			def.line
		);
		lines.push(format!(
			"\tdef_{} [shape=box,style=filled,fillcolor=\"palegreen\",label=\"{}\"];",
			idx,
			escape_dot(&label)
		));
		lines.push(format!("\tscope_{} -> def_{};", def.scope_id, idx));
	}

	let mut def_lookup: HashMap<(SymbolKind, String), usize> = HashMap::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		def_lookup.insert((def.kind, def.name.clone()), idx);
	}

	for (idx, reference) in index.references.iter().enumerate() {
		let label = format!(
			"ref:{}\\n{}\\n{}\\n{}:{}",
			idx,
			symbol_kind_text(reference.kind),
			reference.name,
			reference.path.display(),
			reference.line
		);
		lines.push(format!(
			"\tref_{} [shape=note,style=filled,fillcolor=\"lightblue\",label=\"{}\"];",
			idx,
			escape_dot(&label)
		));
		lines.push(format!("\tscope_{} -> ref_{};", reference.scope_id, idx));
		if let Some(def_idx) = def_lookup.get(&(reference.kind, reference.name.clone())) {
			lines.push(format!(
				"\tref_{} -> def_{} [style=dashed,color=gray40];",
				idx, def_idx
			));
		}
	}

	lines.push("}".to_string());
	lines.join("\n")
}

fn scope_kind_text(kind: ScopeKind) -> &'static str {
	match kind {
		ScopeKind::File => "File",
		ScopeKind::Event => "Event",
		ScopeKind::Decision => "Decision",
		ScopeKind::ScriptedEffect => "ScriptedEffect",
		ScopeKind::Trigger => "Trigger",
		ScopeKind::Effect => "Effect",
		ScopeKind::Loop => "Loop",
		ScopeKind::AliasBlock => "AliasBlock",
		ScopeKind::Block => "Block",
	}
}

fn scope_type_text(scope_type: ScopeType) -> &'static str {
	match scope_type {
		ScopeType::Country => "Country",
		ScopeType::Province => "Province",
		ScopeType::Unknown => "Unknown",
	}
}

fn symbol_kind_text(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "scripted_effect",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}

fn escape_dot(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_json(value: String) -> String {
	value
		.replace('\\', "\\\\")
		.replace('"', "\\\"")
		.replace('\n', "\\n")
}
