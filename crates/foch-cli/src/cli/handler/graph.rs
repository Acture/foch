use crate::cli::arg::{GraphArgs, GraphArtifactFormatArg, GraphModeArg, GraphScopeArg};
use crate::cli::handler::{HandlerResult, resolve_playset_path};
use foch_core::model::SymbolKind;
use foch_engine::{
	CheckRequest, Config, GraphArtifactFormat, GraphBuildOptions, GraphModeSelection,
	GraphRootSelector, GraphScopeSelection, build_runtime_state_for_request,
	run_graph_with_options, run_module_report, write_module_report,
};

const MODULE_REPORT_MAX_ITERS: usize = 20;

pub fn handle_graph(graph_args: &GraphArgs, config: Config) -> HandlerResult {
	if graph_args.modules {
		let playset_path = resolve_playset_path(graph_args.playset_path.as_deref(), &config)?;
		let request = CheckRequest {
			playset_path,
			config,
		};
		let state = build_runtime_state_for_request(&request, !graph_args.no_game_base)?;
		let report = run_module_report(&state.semantic_index, MODULE_REPORT_MAX_ITERS);
		let report_path = graph_args.out.join(".foch").join("module-report.json");
		if let Some(parent) = report_path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		write_module_report(&report_path, &report)?;
		println!("module report written to {}", report_path.display());
		return Ok(0);
	}

	if graph_args.mode == GraphModeArg::Semantic {
		if graph_args.family.is_none() {
			return Err("semantic graph mode requires --family".into());
		}
		if graph_args.root.is_some() {
			return Err("semantic graph mode does not support --root".into());
		}
		if matches!(graph_args.scope, GraphScopeArg::Base | GraphScopeArg::Mods) {
			return Err("semantic graph mode currently supports only workspace/all scope".into());
		}
		if graph_args.format != GraphArtifactFormatArg::Both {
			return Err("semantic graph mode always writes JSON and HTML; omit --format".into());
		}
	}

	let playset_path = resolve_playset_path(graph_args.playset_path.as_deref(), &config)?;
	let request = CheckRequest {
		playset_path,
		config,
	};
	let summary = run_graph_with_options(
		request,
		&graph_args.out,
		GraphBuildOptions {
			include_game_base: !graph_args.no_game_base,
			mode: to_mode(graph_args.mode),
			scope: to_scope(graph_args.scope),
			format: to_format(graph_args.format),
			root: graph_args
				.root
				.as_deref()
				.map(parse_root_selector)
				.transpose()?,
			family: graph_args.family.clone(),
			definition_kinds: graph_args.definition_kinds.clone(),
		},
	)?;
	println!("graph artifacts written to {}", summary.out_dir.display());
	Ok(0)
}

fn parse_root_selector(raw: &str) -> Result<GraphRootSelector, Box<dyn std::error::Error>> {
	let Some((kind, name)) = raw.split_once(':') else {
		return Err("graph root must use <kind:name>".into());
	};
	let kind = match kind {
		"scripted_effect" => SymbolKind::ScriptedEffect,
		"scripted_trigger" => SymbolKind::ScriptedTrigger,
		"event" => SymbolKind::Event,
		"decision" => SymbolKind::Decision,
		"diplomatic_action" => SymbolKind::DiplomaticAction,
		"triggered_modifier" => SymbolKind::TriggeredModifier,
		_ => return Err(format!("unsupported graph root kind {kind}").into()),
	};
	Ok(GraphRootSelector {
		kind,
		name: name.to_string(),
	})
}

fn to_mode(mode: GraphModeArg) -> GraphModeSelection {
	match mode {
		GraphModeArg::Calls => GraphModeSelection::Calls,
		GraphModeArg::Semantic => GraphModeSelection::Semantic,
	}
}

fn to_scope(scope: GraphScopeArg) -> GraphScopeSelection {
	match scope {
		GraphScopeArg::Workspace => GraphScopeSelection::Workspace,
		GraphScopeArg::Base => GraphScopeSelection::Base,
		GraphScopeArg::Mods => GraphScopeSelection::Mods,
		GraphScopeArg::All => GraphScopeSelection::All,
	}
}

fn to_format(format: GraphArtifactFormatArg) -> GraphArtifactFormat {
	match format {
		GraphArtifactFormatArg::Json => GraphArtifactFormat::Json,
		GraphArtifactFormatArg::Dot => GraphArtifactFormat::Dot,
		GraphArtifactFormatArg::Both => GraphArtifactFormat::Both,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::cli::arg::{GraphArtifactFormatArg, GraphModeArg, GraphScopeArg};
	use std::path::PathBuf;
	use tempfile::Builder;

	#[test]
	fn modules_mode_writes_parseable_report_under_foch_dir() {
		let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("target")
			.join("graph-handler");
		std::fs::create_dir_all(&scratch_root).expect("create graph handler scratch root");
		let temp_dir = Builder::new()
			.prefix("modules-mode-")
			.tempdir_in(&scratch_root)
			.expect("create graph handler tempdir");
		let out = temp_dir.path().join("out");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.expect("cli crate has workspace crates dir")
			.join("foch-engine")
			.join("tests")
			.join("fixtures")
			.join("playsets")
			.join("eu4_minimal_passthrough")
			.join("dlc_load.json");

		let exit_code = handle_graph(
			&GraphArgs {
				playset_path: Some(fixture),
				out: out.clone(),
				no_game_base: true,
				modules: true,
				mode: GraphModeArg::Calls,
				scope: GraphScopeArg::All,
				format: GraphArtifactFormatArg::Both,
				root: None,
				family: None,
				definition_kinds: Vec::new(),
			},
			Config::default(),
		)
		.expect("modules graph handler succeeds");

		assert_eq!(exit_code, 0);
		let report_path = out.join(".foch").join("module-report.json");
		let report_json = std::fs::read_to_string(&report_path).expect("read module report");
		let parsed: serde_json::Value =
			serde_json::from_str(&report_json).expect("module report is valid JSON");
		assert!(parsed.get("module_count").is_some());
		assert!(parsed.get("node_count").is_some());
		assert!(parsed.get("mods").is_some());
	}
}
