use crate::cli::arg::{FochCliInputCommands, InputArgs, InputResolveArgs};
use crate::cli::handler::HandlerResult;
use foch::input::{Config, InputRequest, InputResolveSummary, InputSource, resolve_input_summary};

pub fn handle_input(args: &InputArgs, config: Config) -> HandlerResult {
	match &args.command {
		FochCliInputCommands::Resolve(resolve_args) => handle_input_resolve(resolve_args, config),
	}
}

fn handle_input_resolve(args: &InputResolveArgs, config: Config) -> HandlerResult {
	let request = InputRequest::new(InputSource::from_path(args.source_path.clone()), config);
	let summary = resolve_input_summary(&request)?;
	println!("{}", render_input_summary(&summary));
	Ok(0)
}

fn render_input_summary(summary: &InputResolveSummary) -> String {
	let mut lines = vec![
		format!("input: {}", summary.source_path.display()),
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
