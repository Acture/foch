use crate::check::{
	CheckRequest, GraphArtifactFormat, GraphBuildOptions, GraphRootSelector,
	GraphScopeSelection, SymbolKind, run_graph_with_options,
};
use crate::cli::arg::{GraphArgs, GraphArtifactFormatArg, GraphScopeArg};
use crate::cli::config::Config;
use crate::cli::handler::HandlerResult;

pub fn handle_graph(graph_args: &GraphArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: graph_args.playset_path.clone(),
		config,
	};
	let summary = run_graph_with_options(
		request,
		&graph_args.out,
		GraphBuildOptions {
			include_game_base: !graph_args.no_game_base,
			scope: to_scope(graph_args.scope),
			format: to_format(graph_args.format),
			root: graph_args
				.root
				.as_deref()
				.map(parse_root_selector)
				.transpose()?,
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
