use crate::cli::arg::{
	CheckOutputFormat, FochCliInputCommands, InputArgs, InputInspectArgs, InputRepairArgs,
};
use crate::cli::handler::HandlerResult;
use foch::input::{
	BaseDataState, Config, CurrentEu4Input, InputReadiness, InputRequest, InputResolveSummary,
	InputSource, inspect_current_eu4_input, resolve_input_summary,
};
use std::io;
use std::process::{Command, Stdio};

pub fn handle_input(args: &InputArgs, config: Config) -> HandlerResult {
	match &args.command {
		FochCliInputCommands::Inspect(inspect_args) => handle_input_inspect(inspect_args, config),
		FochCliInputCommands::Repair(repair_args) => handle_input_repair(repair_args),
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

fn handle_input_repair(args: &InputRepairArgs) -> HandlerResult {
	let input = inspect_current_eu4_input();
	let targets = repair_targets(&input, &args.mods)?;
	if targets.is_empty() {
		println!("every mod of the current EU4 playset can be analyzed");
		return Ok(0);
	}
	for target in &targets {
		println!(
			"#{} {} {}: {}",
			target.position,
			target.id,
			target.name,
			target.reason.as_deref().unwrap_or("selected")
		);
		match target.workshop_id.as_deref() {
			Some(id) => println!("  {}", workshop_web_url(id)),
			None => println!("  not a Workshop item; repair it outside Steam"),
		}
	}
	if args.open {
		let opened = open_workshop_pages(&targets)?;
		println!(
			"opened {opened} Workshop page(s) in Steam: unsubscribe and subscribe again, wait for the download, then inspect again"
		);
	}
	Ok(0)
}

/// A playset mod to repair, as `foch input repair` and bare `foch` list it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairTarget {
	pub position: usize,
	pub id: String,
	pub name: String,
	/// Why it cannot be analyzed; `None` for a working mod named explicitly.
	pub reason: Option<String>,
	/// The numeric Workshop id Steam can open, if it is a Workshop item.
	pub workshop_id: Option<String>,
}

/// The mods to repair: those named by Workshop id or `#POSITION`, or every
/// mod that cannot be analyzed when none is named.
pub fn repair_targets(
	input: &CurrentEu4Input,
	named: &[String],
) -> Result<Vec<RepairTarget>, String> {
	let mods = input
		.playset
		.as_ref()
		.map(|playset| playset.mods.as_slice())
		.unwrap_or_default();
	let target = |playset_mod: &foch::input::DetectedPlaysetMod| RepairTarget {
		position: playset_mod.position,
		id: playset_mod.id.clone(),
		name: playset_mod.name.clone(),
		reason: playset_mod.source_error.clone(),
		workshop_id: playset_mod
			.workshop_id
			.clone()
			.filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())),
	};
	if named.is_empty() {
		return Ok(mods
			.iter()
			.filter(|playset_mod| playset_mod.source_error.is_some())
			.map(target)
			.collect());
	}
	named
		.iter()
		.map(|name| {
			mods.iter()
				.find(|playset_mod| match name.strip_prefix('#') {
					Some(position) => position.parse() == Ok(playset_mod.position),
					None => playset_mod.id == *name,
				})
				.map(target)
				.ok_or_else(|| {
					format!(
						"`{name}` names no mod of the current EU4 playset; see `foch input inspect`"
					)
				})
		})
		.collect()
}

pub fn workshop_web_url(workshop_id: &str) -> String {
	format!("https://steamcommunity.com/sharedfiles/filedetails/?id={workshop_id}")
}

/// Open each target's Workshop page in the Steam client. Returns how many
/// opened; targets that are not Workshop items are skipped.
pub fn open_workshop_pages(targets: &[RepairTarget]) -> io::Result<usize> {
	let mut opened = 0;
	for workshop_id in targets
		.iter()
		.filter_map(|target| target.workshop_id.as_deref())
	{
		open_url(&format!("steam://url/CommunityFilePage/{workshop_id}"))?;
		opened += 1;
	}
	Ok(opened)
}

/// Hand a URL to the desktop's handler. Callers pass only URLs built from
/// numeric Workshop ids.
fn open_url(url: &str) -> io::Result<()> {
	let mut command = if cfg!(windows) {
		let mut command = Command::new("rundll32");
		command.args(["url.dll,FileProtocolHandler", url]);
		command
	} else if cfg!(target_os = "macos") {
		let mut command = Command::new("open");
		command.arg(url);
		command
	} else {
		let mut command = Command::new("xdg-open");
		command.arg(url);
		command
	};
	command
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
		.map(|_| ())
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
