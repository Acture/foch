use crate::cli::arg::{GraphArgs, GraphArtifactFormatArg, GraphModeArg, GraphScopeArg};
use crate::cli::handler::HandlerResult;
use foch_core::model::SymbolKind;
use foch_engine::{
	CheckRequest, Config, GraphArtifactFormat, GraphBuildOptions, GraphModeSelection,
	GraphRootSelector, GraphScopeSelection, run_graph_with_options,
};

pub fn handle_graph(graph_args: &GraphArgs, config: Config) -> HandlerResult {
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

	let request = CheckRequest {
		playset_path: graph_args.playset_path.clone(),
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
