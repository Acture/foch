//! `foch test`: input and output around `foch-test`.
//!
//! This module discovers files, protects output directories, launches the
//! runner and guards the user's real game profile. Every decision about
//! cases, sessions and verdicts belongs to `foch-test`.

use super::HandlerResult;
use crate::cli::arg::{CheckOutputFormat, TestArgs};
use foch::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};
use foch::input::load_config_read_only;
use foch_annotation::builtin::SCHEMAS;
use foch_annotation::value::scalar;
use foch_runner::{Installation, RunOptions};
use foch_test::judge::CaseResult;
use foch_test::model::RunId;
use foch_test::plan::{IgnoredMode, Isolation, Plan, SessionId};
use foch_test::report::EnvironmentRecord;
use foch_test::select::{Expr, NodeIdPattern};
use foch_test::{
	Bundle, Diagnostic, ExpandOptions, GroupOptions, ProjectFacts, RelPath, Report, RunArtifacts,
	RunContext, Selector, SourceFile, SourceKind, Status, blocks_run, check_capabilities, collect,
	compile, expand, group, judge, lint, reconcile,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn handle_test(args: &TestArgs) -> HandlerResult {
	if let Some(name) = &args.api {
		return print_api(name, args.format);
	}
	let selection = Selection {
		filter: args.filter.as_deref(),
		marks: args.marks.as_deref(),
		ignored: if args.ignored {
			IgnoredMode::Only
		} else if args.include_ignored {
			IgnoredMode::Include
		} else {
			IgnoredMode::Exclude
		},
	};
	let capabilities = foch_runner::capabilities();
	let planned = plan(
		&args.path,
		&selection,
		args.isolate,
		capabilities.shared_sessions,
	)?;
	if args.collect_only || blocks_run(&planned.diagnostics, args.run_anyway) {
		print_collection(&planned, args.format)?;
		let failed =
			blocks_run(&planned.diagnostics, args.run_anyway) || planned.plan.cases.is_empty();
		return Ok(i32::from(failed));
	}
	if planned.plan.cases.is_empty() {
		print_collection(&planned, args.format)?;
		return Ok(1);
	}
	if args.no_run {
		return write_bundles(args, &planned);
	}

	let run_id = new_run_id()?;
	let context = RunContext {
		run_id: run_id.clone(),
		mod_name: planned.mod_name.clone(),
	};
	let mut bundles = Vec::new();
	for session in &planned.plan.sessions {
		let bundle = compile(&planned.plan, session.id, &context)?;
		check_capabilities(&bundle, &capabilities)?;
		bundles.push(bundle);
	}

	let config = load_config_read_only()?;
	let game_root = foch_runner::locate_game(&config, args.game_path.as_deref())?;
	let real_user_dir = args.eu4_user_dir.clone().or_else(default_eu4_user_dir);
	let options = RunOptions {
		timeout: Duration::from_secs(args.timeout),
	};
	let requested_output = match &args.out {
		Some(output) => output.clone(),
		// System temporary directories may use aliases such as macOS /var.
		// Resolve this trusted default before enforcing explicit output rules.
		None => fs::canonicalize(std::env::temp_dir())?.join(format!("foch-tests-{run_id}")),
	};
	let output = prepare_output(&requested_output, &planned.root)?;
	// Keep the game's own files within MAX_PATH: a short base, not `output`.
	let runtime_base = fs::canonicalize(std::env::temp_dir())?.join("foch-rt");
	fs::create_dir_all(&runtime_base)?;
	let installation = Installation {
		game_root: game_root.clone(),
		runtime_base,
		real_user_dir,
	};

	let mut sessions = Vec::new();
	let mut results = Vec::new();
	for bundle in &bundles {
		let directory = output.join(format!("session_{:04}", bundle.session.0 + 1));
		let (record, session_results) = run_one_session(
			&planned.plan,
			bundle,
			&planned.root,
			&directory,
			&installation,
			&options,
		)?;
		sessions.push(record);
		results.extend(session_results);
	}

	// A failure in a shared session is confirmed alone before it is reported.
	let suspicious: Vec<_> = results
		.iter()
		.filter(|result| result.isolation == Isolation::Shared && !result.status.is_success())
		.map(|result| result.index)
		.collect();
	if !suspicious.is_empty() {
		let rerun = planned.plan.isolated_rerun(&suspicious);
		for session in &rerun.sessions {
			let bundle = compile(&rerun, session.id, &context)?;
			let directory = output.join(format!("rerun_{:04}", session.id.0 + 1));
			let (record, mut isolated) = run_one_session(
				&rerun,
				&bundle,
				&planned.root,
				&directory,
				&installation,
				&options,
			)?;
			sessions.push(record);
			let isolated = isolated.remove(0);
			if let Some(shared) = results
				.iter_mut()
				.find(|result| result.index == isolated.index)
			{
				*shared = reconcile(shared, isolated);
			}
		}
	}

	let report = Report::new(
		run_id,
		results,
		planned.plan.not_run.clone(),
		EnvironmentRecord {
			mods: vec![planned.mod_name.clone()],
			runner: Some(format!("built-in {}", game_root.display())),
			..EnvironmentRecord::default()
		},
	);
	let view = RunView {
		source_root: &planned.root,
		output: &output,
		success: report.exit_code() == 0,
		summary: report.summary(),
		sessions: &sessions,
		report: &report,
	};
	write_new(
		&output.join("result.json"),
		&serde_json::to_vec_pretty(&view)?,
	)?;
	write_new(&output.join("junit.xml"), report.to_junit().as_bytes())?;
	if args.format == CheckOutputFormat::Json {
		print_json(&view)?;
	} else {
		print_results(&report, &output);
	}
	Ok(report.exit_code())
}

/// `--no-run`: write every session's bundle without launching the game.
fn write_bundles(args: &TestArgs, planned: &Planned) -> HandlerResult {
	let run_id = new_run_id()?;
	let context = RunContext {
		run_id: run_id.clone(),
		mod_name: planned.mod_name.clone(),
	};
	let mut compiled = Vec::new();
	for session in &planned.plan.sessions {
		compiled.push(compile(&planned.plan, session.id, &context)?);
	}
	let requested_output = match &args.out {
		Some(output) => output.clone(),
		None => fs::canonicalize(std::env::temp_dir())?.join(format!("foch-tests-{run_id}")),
	};
	let output = prepare_output(&requested_output, &planned.root)?;
	let mut bundles = Vec::new();
	for bundle in &compiled {
		let directory = output.join(format!("session_{:04}", bundle.session.0 + 1));
		bundles.push(materialize(bundle, &planned.root, &directory)?);
	}
	if args.format == CheckOutputFormat::Json {
		print_json(&serde_json::json!({ "run_id": run_id, "output": output, "bundles": bundles }))?;
	} else {
		for bundle in bundles {
			println!("{}", bundle.display());
		}
	}
	Ok(0)
}

fn print_api(name: &str, format: CheckOutputFormat) -> HandlerResult {
	let schemas: Vec<_> = SCHEMAS
		.iter()
		.filter(|schema| {
			name.is_empty()
				|| schema.name == name
				|| schema.params.iter().any(|param| param.name == name)
		})
		.collect();
	if schemas.is_empty() {
		return Err(format!("unknown annotation or parameter: {name}").into());
	}
	if format == CheckOutputFormat::Json {
		print_json(&serde_json::json!({ "annotations": schemas }))?;
		return Ok(0);
	}
	for schema in schemas {
		println!("#{}(...)\n  {}\n", schema.name, schema.description);
		for param in schema
			.params
			.iter()
			.filter(|param| name.is_empty() || name == schema.name || param.name == name)
		{
			println!(
				"  {} ({}){}: {}\n    {} = {}",
				param.name,
				param.value_type.describe(),
				if param.required { " required" } else { "" },
				param.description,
				param.name,
				param.example
			);
		}
		if let Some(positional) = schema.positional {
			println!(
				"  {}: #{}({})",
				positional.description, schema.name, positional.example
			);
		}
		println!();
	}
	Ok(0)
}

struct Selection<'a> {
	filter: Option<&'a str>,
	marks: Option<&'a str>,
	ignored: IgnoredMode,
}

struct Planned {
	root: PathBuf,
	mod_name: String,
	plan: Plan,
	diagnostics: Vec<Diagnostic>,
}

/// Discover, collect, expand, lint and group. Diagnostics are returned for
/// the caller to print; a plan is always produced so collection can be shown.
fn plan(input: &Path, selection: &Selection, isolate: bool, shared: bool) -> Result<Planned> {
	// `PATH::node` selects inside a file, as in pytest.
	let text = input.to_string_lossy();
	let (path, node) = match text.split_once("::") {
		Some((path, node)) => (PathBuf::from(path), Some(node.to_string())),
		None => (input.to_path_buf(), None),
	};
	let (root, files) = discover(&path)?;
	let mod_name = mod_name(&root);
	let mut sources = Vec::new();
	for file in files {
		let relative = file
			.strip_prefix(&root)?
			.to_string_lossy()
			.replace('\\', "/");
		sources.push(SourceFile {
			mod_name: mod_name.clone(),
			path: RelPath::new(&relative)?,
			// The tests/ directory is not collected until EU4 is verified to
			// ignore it and merge treats it as test-only content.
			kind: SourceKind::Inline,
			text: fs::read_to_string(&file)?,
		});
	}
	let collection = collect(&sources);
	let mut selector = Selector {
		keyword: selection.filter.map(Expr::parse).transpose()?,
		marks: selection.marks.map(Expr::parse).transpose()?,
		..Selector::default()
	};
	if let Some(node) = node {
		let file = path.canonicalize()?;
		let relative = file
			.strip_prefix(&root)?
			.to_string_lossy()
			.replace('\\', "/");
		selector
			.nodes
			.push(NodeIdPattern::parse(&format!("{relative}::{node}"))?);
	} else if path.is_file() {
		let relative = path
			.canonicalize()?
			.strip_prefix(&root)?
			.to_string_lossy()
			.replace('\\', "/");
		selector.nodes.push(NodeIdPattern::parse(&relative)?);
	}
	let mut diagnostics = collection.diagnostics.clone();
	let options = ExpandOptions {
		selector,
		ignored: selection.ignored,
		..ExpandOptions::default()
	};
	let plan = match expand(&collection, &options) {
		Ok(expanded) => {
			let linted = lint(&expanded, &ProjectFacts::default());
			diagnostics.extend(linted.diagnostics.iter().cloned());
			group(
				expanded,
				&linted,
				&GroupOptions {
					force_isolate: isolate,
					runner_shared_sessions: shared,
					..GroupOptions::default()
				},
			)
		}
		Err(errors) => {
			diagnostics.extend(errors);
			Plan {
				cases: Vec::new(),
				not_run: Vec::new(),
				sessions: Vec::new(),
				alone_reasons: BTreeMap::new(),
				events: Vec::new(),
				helpers: Vec::new(),
			}
		}
	};
	Ok(Planned {
		root,
		mod_name,
		plan,
		diagnostics,
	})
}

fn discover(path: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
	for ancestor in std::path::absolute(path)?.ancestors() {
		if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
			return Err(format!(
				"test input must not traverse a symlink: {}",
				ancestor.display()
			)
			.into());
		}
	}
	let metadata = fs::symlink_metadata(path)?;
	let path = fs::canonicalize(path)?;
	if metadata.is_file() {
		if path.extension().and_then(|v| v.to_str()) != Some("txt") {
			return Err("explicit test input must be an event .txt file".into());
		}
		let parent = path.parent().ok_or("test file has no parent")?;
		let root = parent
			.ancestors()
			.find(|p| p.file_name().is_some_and(|v| v == "events"))
			.and_then(Path::parent)
			.unwrap_or(parent)
			.to_path_buf();
		// Collect the whole mod so fire targets and grouping see every event;
		// the file itself is selected by node pattern.
		let mut files = event_files(&root)?;
		if !files.contains(&path) {
			files.push(path);
		}
		return Ok((root, files));
	}
	if !metadata.is_dir() {
		return Err("test input must be a mod directory or an event .txt file".into());
	}
	let root = if path.file_name().is_some_and(|v| v == "events") {
		path.parent()
			.ok_or("events directory has no parent")?
			.to_path_buf()
	} else {
		path
	};
	let files = event_files(&root)?;
	Ok((root, files))
}

fn event_files(root: &Path) -> Result<Vec<PathBuf>> {
	let events = root.join("events");
	let mut files = Vec::new();
	match fs::symlink_metadata(&events) {
		Ok(metadata) if metadata.file_type().is_symlink() => {
			return Err("events directory must not be a symlink".into());
		}
		Ok(_) => {
			for entry in WalkDir::new(events).follow_links(false) {
				let entry = entry?;
				if entry.file_type().is_file()
					&& entry.path().extension().and_then(|v| v.to_str()) == Some("txt")
				{
					files.push(entry.into_path());
				}
			}
		}
		Err(err) if err.kind() == io::ErrorKind::NotFound => {}
		Err(err) => return Err(err.into()),
	}
	files.sort();
	Ok(files)
}

/// The descriptor's `name`, else the directory name.
fn mod_name(root: &Path) -> String {
	let from_descriptor = fs::read_to_string(root.join("descriptor.mod"))
		.ok()
		.and_then(|text| {
			let parsed = parse_clausewitz_content(
				foch::model::GamePath::new("descriptor.mod").unwrap(),
				&text,
			);
			parsed
				.ast
				.statements
				.iter()
				.find_map(|statement| match statement {
					AstStatement::Assignment { key, value, .. } if key == "name" => {
						scalar(value).ok()
					}
					_ => None,
				})
		});
	from_descriptor
		.filter(|name| !name.trim().is_empty() && !name.contains("::"))
		.or_else(|| {
			root.file_name()
				.map(|name| name.to_string_lossy().into_owned())
		})
		.unwrap_or_else(|| "mod".into())
}

#[derive(Serialize)]
struct CaseView<'a> {
	node_id: String,
	kind: &'static str,
	origin: &'a foch_test::source::SourceSpan,
	start: &'a foch_test::model::Start,
	ai: foch_test::model::AiMode,
	marks: &'a foch_test::model::Marks,
	session: Option<SessionId>,
	isolation: Option<Isolation>,
	alone_reason: Option<&'a foch_test::plan::AloneReason>,
}

fn print_collection(planned: &Planned, format: CheckOutputFormat) -> Result<()> {
	let plan = &planned.plan;
	let views: Vec<_> = plan
		.cases
		.iter()
		.map(|planned_case| {
			let session = plan
				.sessions
				.iter()
				.find(|session| session.cases.contains(&planned_case.index));
			CaseView {
				node_id: planned_case.case.node.to_string(),
				kind: if planned_case.case.is_smoke() {
					"smoke"
				} else {
					"assertion"
				},
				origin: &planned_case.case.origin,
				start: &planned_case.case.start,
				ai: planned_case.case.ai,
				marks: &planned_case.case.marks,
				session: session.map(|session| session.id),
				isolation: session.map(|session| session.isolation),
				alone_reason: plan.alone_reasons.get(&planned_case.index),
			}
		})
		.collect();
	if format == CheckOutputFormat::Json {
		print_json(&serde_json::json!({
			"cases": views,
			"not_run": plan.not_run,
			"diagnostics": planned.diagnostics,
		}))?;
	} else {
		for view in &views {
			let session = match (view.session, view.isolation, view.alone_reason) {
				(Some(id), Some(Isolation::Shared), _) => format!("session {} shared", id.0 + 1),
				(Some(id), _, Some(reason)) => {
					format!("session {} alone: {}", id.0 + 1, alone_label(reason))
				}
				(Some(id), ..) => format!("session {}", id.0 + 1),
				(None, ..) => "not planned".into(),
			};
			let ai = match view.ai {
				foch_test::model::AiMode::On => "on",
				foch_test::model::AiMode::Off => "off",
			};
			println!(
				"{}\t{}\t{}\t{} {}\tai={ai}\t{session}",
				view.kind, view.origin, view.node_id, view.start.date, view.start.tag
			);
		}
		for not_run in &plan.not_run {
			println!("{:?}\t{}\t{}", not_run.reason, not_run.origin, not_run.node);
		}
		for diagnostic in &planned.diagnostics {
			eprintln!("{diagnostic}");
		}
	}
	if plan.cases.is_empty() {
		eprintln!("no tests selected (check PATH, -k and -m)");
	}
	Ok(())
}

/// `{"reason": "unknown_effect", "detail": "..."}` -> `unknown effect (...)`.
fn alone_label(reason: &foch_test::plan::AloneReason) -> String {
	let value = serde_json::to_value(reason).unwrap_or_default();
	let mut label = value["reason"]
		.as_str()
		.unwrap_or_default()
		.replace('_', " ");
	if let Some(detail) = value["detail"].as_str() {
		label.push_str(&format!(" ({detail})"));
	}
	label
}

fn print_results(report: &Report, output: &Path) {
	for result in &report.results {
		println!(
			"{}\t{}\t{}\t{} {}\t{:.1}s",
			status_label(result.status),
			result.origin,
			result.node,
			result.start.date,
			result.start.tag,
			result.timing.wall_ms as f64 / 1000.0
		);
		for failure in &result.failures {
			match &failure.span {
				Some(span) => println!("  {span}  {}: {}", failure.check, failure.message),
				None => println!("  {}: {}", failure.check, failure.message),
			}
		}
		if !result.missing_checks.is_empty() {
			println!("  missing: {}", result.missing_checks.join(", "));
		}
		for note in &result.notes {
			println!("  {note}");
		}
	}
	let summary = report.summary();
	println!(
		"{} passed, {} smoke, {} failed, {} errors, {} skipped in {:.1}s",
		summary.passed + summary.xfailed,
		summary.smoke,
		summary.failed + summary.xpassed,
		summary.setup_error + summary.incomplete + summary.protocol_error + summary.runtime_error,
		summary.skipped + summary.ignored,
		summary.wall_ms as f64 / 1000.0
	);
	println!("Artifacts: {}", output.display());
}

fn status_label(status: Status) -> String {
	serde_json::to_value(status)
		.ok()
		.and_then(|value| value["status"].as_str().map(str::to_uppercase))
		.unwrap_or_else(|| format!("{status:?}"))
}

#[derive(Serialize)]
struct RunView<'a> {
	source_root: &'a Path,
	output: &'a Path,
	success: bool,
	summary: foch_test::report::Summary,
	sessions: &'a [SessionRecord],
	#[serde(flatten)]
	report: &'a Report,
}

/// What the CLI observed about one game launch.
#[derive(Clone, Debug, Serialize)]
struct SessionRecord {
	cases: Vec<String>,
	launch: String,
	#[serde(flatten)]
	exit: foch_test::RunnerExit,
}

/// Build one session's test layer, launch the game through `foch-runner`,
/// judge the log, and record what was launched.
fn run_one_session(
	plan: &Plan,
	bundle: &Bundle,
	source_root: &Path,
	directory: &Path,
	installation: &Installation,
	options: &RunOptions,
) -> Result<(SessionRecord, Vec<CaseResult>)> {
	// Write the full bundle (test layer, commands, manifest) for inspection;
	// the runner reuses the test layer it leaves in `directory`.
	materialize(bundle, source_root, directory)?;
	let outcome = foch_runner::run_session(bundle, source_root, directory, installation, options)?;
	let results = judge(
		bundle,
		plan,
		&RunArtifacts {
			game_log: outcome.game_log.as_deref(),
			error_log: outcome.error_log.as_deref(),
			exit: outcome.exit.clone(),
			timing: outcome.timing,
			profile_untouched: outcome.profile_untouched,
		},
	);
	let record = SessionRecord {
		cases: bundle
			.cases
			.iter()
			.map(|&index| plan.case(index).case.node.to_string())
			.collect(),
		launch: outcome.launch_detail,
		exit: outcome.exit,
	};
	Ok((record, results))
}

/// The player's real EU4 user directory under their Documents folder.
fn default_eu4_user_dir() -> Option<PathBuf> {
	dirs::document_dir().map(|documents| {
		documents
			.join("Paradox Interactive")
			.join("Europa Universalis IV")
	})
}

fn new_run_id() -> Result<RunId> {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	Ok(RunId::parse(&format!("{}_{nanos}", std::process::id()))?)
}

fn print_json(value: &impl Serialize) -> Result<()> {
	let stdout = io::stdout();
	let mut stdout = stdout.lock();
	serde_json::to_writer_pretty(&mut stdout, value)?;
	writeln!(stdout)?;
	Ok(())
}

// Resolve nonexistent output components before writing, so lexical '..' and
// existing symlinks cannot route a new directory into the source mod.
fn resolved_output(path: &Path) -> Result<PathBuf> {
	let absolute = std::path::absolute(path)?;
	let mut resolved = PathBuf::new();
	for component in absolute.components() {
		match component {
			Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
			Component::RootDir => resolved.push(component.as_os_str()),
			Component::CurDir => {}
			Component::ParentDir => {
				resolved.pop();
			}
			component => {
				resolved.push(component.as_os_str());
				match fs::symlink_metadata(&resolved) {
					Ok(metadata) if metadata.file_type().is_symlink() => {
						return Err(format!(
							"output must not traverse a symlink: {}",
							resolved.display()
						)
						.into());
					}
					Ok(_) => {}
					Err(err) if err.kind() == io::ErrorKind::NotFound => {}
					Err(err) => {
						return Err(format!(
							"cannot inspect output component {}: {err}",
							resolved.display()
						)
						.into());
					}
				}
			}
		}
	}
	let mut missing = Vec::new();
	loop {
		match fs::symlink_metadata(&resolved) {
			Ok(_) => break,
			Err(error) if error.kind() == io::ErrorKind::NotFound => {
				missing.push(
					resolved
						.file_name()
						.ok_or("output has no existing ancestor")?
						.to_os_string(),
				);
				resolved.pop();
			}
			Err(error) => return Err(error.into()),
		}
	}
	// Canonicalize only the closest existing ancestor. On Windows, opening an
	// otherwise traversable ancestor (such as a protected home) may be denied.
	let mut resolved = fs::canonicalize(resolved)?;
	for component in missing.into_iter().rev() {
		resolved.push(component);
	}
	Ok(resolved)
}

fn prepare_output(path: &Path, source_root: &Path) -> Result<PathBuf> {
	let output = resolved_output(path)?;
	if output.starts_with(source_root) {
		return Err("test output must be outside the source mod".into());
	}
	if output.exists() {
		if !output.is_dir() || fs::read_dir(&output)?.next().transpose()?.is_some() {
			return Err(format!("test output must be new or empty: {}", output.display()).into());
		}
	} else {
		if let Some(parent) = output.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::create_dir(&output)?;
	}
	Ok(fs::canonicalize(output)?)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
	let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
	file.write_all(bytes)?;
	Ok(())
}

/// Writes one session's bundle and returns the absolute `bundle.json` path.
fn materialize(bundle: &Bundle, source_root: &Path, directory: &Path) -> Result<PathBuf> {
	fs::create_dir(directory)?;
	for (relative, content) in &bundle.files {
		let relative = Path::new(relative);
		if relative
			.components()
			.any(|c| !matches!(c, Component::Normal(_)))
		{
			return Err("compiler produced an unsafe artifact path".into());
		}
		let destination = directory.join(relative);
		fs::create_dir_all(destination.parent().ok_or("artifact has no parent")?)?;
		write_new(&destination, content.as_bytes())?;
	}
	let mut manifest = serde_json::to_value(bundle)?;
	manifest["source_root"] = serde_json::to_value(source_root)?;
	let path = directory.join("bundle.json");
	write_new(&path, &serde_json::to_vec_pretty(&manifest)?)?;
	Ok(path)
}
