use crate::cli::arg::{CheckOutputFormat, FochCliInputCommands, InputArgs, InputInspectArgs};
use crate::cli::handler::HandlerResult;
use foch::input::{
	BaseDataState, Config, CurrentEu4Input, InputReadiness, InputRequest, InputResolveSummary,
	InputSource, inspect_current_eu4_input, resolve_input_summary,
};

pub fn handle_input(args: &InputArgs, config: Config) -> HandlerResult {
	match &args.command {
		FochCliInputCommands::Inspect(inspect_args) => handle_input_inspect(inspect_args, config),
	}
}

fn handle_input_inspect(args: &InputInspectArgs, config: Config) -> HandlerResult {
	let Some(source_path) = &args.source_path else {
		let input = inspect_current_eu4_input();
		match args.format {
			CheckOutputFormat::Text => println!("{}", render_current_input(&input)),
			CheckOutputFormat::Json => println!("{}", serde_json::to_string_pretty(&input)?),
		}
		return Ok(match input.readiness {
			InputReadiness::Blocked => 2,
			InputReadiness::Ready | InputReadiness::ReadyWithOmissions => 0,
		});
	};
	if args.format == CheckOutputFormat::Json {
		return Err("`--format json` describes the current EU4 input; omit INPUT_SOURCE".into());
	}
	let request = InputRequest::new(InputSource::from_path(source_path.clone()), config);
	let summary = resolve_input_summary(&request)?;
	println!("{}", render_input_summary(&summary));
	Ok(0)
}

/// The current EU4 input as bare `foch` shows it first: the game, base data,
/// each playset mod with the position and id `foch merge --exclude` takes,
/// and every issue.
fn render_current_input(input: &CurrentEu4Input) -> String {
	let mut lines = vec![
		format!(
			"readiness: {}",
			match input.readiness {
				InputReadiness::Ready => "ready",
				InputReadiness::ReadyWithOmissions => "ready_with_omissions",
				InputReadiness::Blocked => "blocked",
			}
		),
		format!(
			"game: {} {}",
			input.game.name,
			input.game.version.as_deref().unwrap_or("<unknown version>")
		),
		format!(
			"base_data: {} {}",
			match input.base_data.state {
				BaseDataState::Ready => "ready",
				BaseDataState::Missing => "missing",
				BaseDataState::Stale => "stale",
			},
			input.base_data.detail
		),
	];
	if let Some(playset) = &input.playset {
		lines.push(format!(
			"playset: {} ({} mods) {}",
			playset.name,
			playset.mods.len(),
			playset.source_path.display()
		));
		for playset_mod in &playset.mods {
			let status = match &playset_mod.source_error {
				None => "ok".to_string(),
				Some(error) => format!("cannot_analyze: {error}"),
			};
			lines.push(format!(
				"  #{} {} {} [{status}]",
				playset_mod.position, playset_mod.id, playset_mod.name
			));
		}
	} else {
		lines.push("playset: <none>".to_string());
	}
	if input.issues.is_empty() {
		lines.push("issues: none".to_string());
	} else {
		lines.push("issues:".to_string());
		for issue in &input.issues {
			lines.push(format!("  - {}: {}", issue.title, issue.detail));
			if let Some(action) = &issue.action {
				lines.push(format!("    action: {action}"));
			}
		}
	}
	lines.join("\n")
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
