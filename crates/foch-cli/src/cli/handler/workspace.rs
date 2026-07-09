use crate::cli::arg::{FochCliWorkspaceCommands, WorkspaceArgs, WorkspaceResolveArgs};
use crate::cli::handler::HandlerResult;
use foch_engine::{
	CheckRequest, Config, WorkspaceResolveSummary, WorkspaceSource, resolve_workspace_summary,
};

pub fn handle_workspace(args: &WorkspaceArgs, config: Config) -> HandlerResult {
	match &args.command {
		FochCliWorkspaceCommands::Resolve(resolve_args) => {
			handle_workspace_resolve(resolve_args, config)
		}
	}
}

fn handle_workspace_resolve(args: &WorkspaceResolveArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		source: WorkspaceSource::from_path(args.source_path.clone()),
		config,
	};
	let summary = resolve_workspace_summary(&request)?;
	println!("{}", render_workspace_summary(&summary));
	Ok(0)
}

fn render_workspace_summary(summary: &WorkspaceResolveSummary) -> String {
	let mut lines = vec![
		format!("workspace: {}", summary.source_path.display()),
		format!("game: {}", summary.game.key()),
		format!(
			"game_root: {}",
			summary
				.game_root
				.as_ref()
				.map(|path| path.display().to_string())
				.unwrap_or_else(|| "<unresolved>".to_string())
		),
		"mods:".to_string(),
	];
	for mod_item in &summary.mods {
		let display = mod_item
			.display_name
			.as_deref()
			.filter(|value| !value.trim().is_empty())
			.unwrap_or(&mod_item.mod_id);
		let steam = mod_item
			.steam_id
			.as_deref()
			.map(|value| format!(" steam_id={value}"))
			.unwrap_or_default();
		let root = mod_item
			.root_path
			.as_ref()
			.map(|path| path.display().to_string())
			.unwrap_or_else(|| "<missing>".to_string());
		let descriptor = mod_item
			.descriptor_error
			.as_deref()
			.map(|error| format!(" descriptor_error={error}"))
			.unwrap_or_default();
		lines.push(format!(
			"  - id={} name={}{} path={}{}",
			mod_item.mod_id, display, steam, root, descriptor
		));
	}
	lines.join("\n")
}
