//! `foch test`: input and output around `foch-test`.
//!
//! This module discovers files, protects output directories, launches the
//! runner and guards the user's real game profile. Every decision about
//! cases, sessions and verdicts belongs to `foch-test`.

use super::HandlerResult;
use crate::cli::arg::{CheckOutputFormat, TestArgs};
use foch::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};
use foch::input::load_config_read_only;
use foch::playset::descriptor::{load_descriptor, load_launcher_descriptor};
use foch::project::{Project, ProjectImportKind, ProjectMod};
use foch_annotation::builtin::SCHEMAS;
use foch_annotation::value::scalar;
use foch_runtime::runner::{DependencyMod, Installation, RunOptions};
use foch_test::judge::CaseResult;
use foch_test::model::{EventId, RunId, Tag};
use foch_test::plan::{IgnoredMode, Isolation, Plan, SessionId};
use foch_test::report::EnvironmentRecord;
use foch_test::select::{Expr, NodeIdPattern};
use foch_test::{
	Bundle, ContentLayer, Diagnostic, ExpandOptions, GroupOptions, InterpretOptions,
	Interpretation, LayerFile, LoadMod, Outcome, ProjectFacts, RelPath, Report, RunArtifacts,
	RunContext, Selector, SemanticsCatalog, SourceFile, SourceKind, Status, blocks_run,
	check_capabilities, collect, compile, complete_load_order, definition_facts, expand, group,
	interpret, judge, lint, overlay_decisions, overlay_history, overlay_subject_types, reconcile,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// Runtime layers left by a run that was killed before cleanup are swept once
/// they are older than this, so a concurrent run's fresh layer is never hit.
const STALE_LAYER_GRACE: Duration = Duration::from_secs(2 * 3600);

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
	let capabilities = foch_runtime::runner::capabilities();
	let planned = plan(args, &selection, capabilities.shared_sessions)?;
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
	if args.offline {
		return interpret_report(args, &planned);
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
	let game_root = foch_runtime::runner::locate_game(&config, args.game_path.as_deref())?;
	let real_user_dir = args.eu4_user_dir.clone().or_else(default_eu4_user_dir);
	// Ctrl-C stops the current launch gracefully: the flag is observed by the
	// runner's watch loop, which stops the game and lets the layer tear down.
	let cancel = Arc::new(AtomicBool::new(false));
	install_cancel_handler(&cancel);
	let host = args
		.with_plugins
		.as_ref()
		.map(
			|playset| -> std::result::Result<foch_runtime::runner::HostLaunch, String> {
				let directory = std::env::current_exe()
					.ok()
					.and_then(|exe| exe.parent().map(Path::to_path_buf))
					.ok_or("foch executable has no parent directory")?;
				let host_dll = foch_runtime::host::resolve(&directory, None)
					.map_err(|error| error.to_string())?;
				Ok(foch_runtime::runner::HostLaunch {
					playset: playset.clone(),
					host_dll,
				})
			},
		)
		.transpose()
		.map_err(Box::<dyn std::error::Error>::from)?;
	let options = RunOptions {
		timeout: Duration::from_secs(args.timeout),
		cancel: cancel.clone(),
		host,
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
	// Clear layers a previously killed run could not tear down itself.
	foch_runtime::runner::sweep_runtime_base(&runtime_base, STALE_LAYER_GRACE);
	let installation = Installation {
		game_root: game_root.clone(),
		runtime_base,
		real_user_dir,
	};

	let mut sessions = Vec::new();
	let mut results = Vec::new();
	for bundle in &bundles {
		if cancel.load(Ordering::Relaxed) {
			break;
		}
		let directory = output.join(format!("session_{:04}", bundle.session.0 + 1));
		let (record, session_results) = run_one_session(
			&planned.plan,
			bundle,
			&planned.root,
			&planned.ordered_mods,
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
	if !suspicious.is_empty() && !cancel.load(Ordering::Relaxed) {
		let rerun = planned.plan.isolated_rerun(&suspicious);
		for session in &rerun.sessions {
			if cancel.load(Ordering::Relaxed) {
				break;
			}
			let bundle = compile(&rerun, session.id, &context)?;
			let directory = output.join(format!("rerun_{:04}", session.id.0 + 1));
			let (record, mut isolated) = run_one_session(
				&rerun,
				&bundle,
				&planned.root,
				&planned.ordered_mods,
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
		playset: &planned.load_order,
		unresolved_dependencies: &planned.unresolved,
		check_scope: &planned.check_scope,
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

fn replace_paths_of(root: &Path) -> Result<Vec<String>> {
	let path = root.join("descriptor.mod");
	if !path.is_file() {
		return Ok(Vec::new());
	}
	let descriptor = foch::playset::descriptor::load_descriptor(&path)?;
	Ok(descriptor
		.replace_path
		.iter()
		.map(|item| item.to_string())
		.collect())
}

/// Decisions, history, country tags and events this root ships. Files inside
/// the layer stay in path order; the caller orders the layers. Tag and event
/// facts are parsed from these same texts.
fn read_content_layer(root: &Path, replace_paths: Vec<String>) -> Result<ContentLayer> {
	let mut files = Vec::new();
	for directory in [
		"decisions",
		"history/countries",
		"history/provinces",
		"common/subject_types",
		"common/country_tags",
		"events",
	] {
		for path in directory_files(root, directory)? {
			let relative = path
				.strip_prefix(root)?
				.to_string_lossy()
				.replace('\\', "/");
			let bytes = fs::read(&path)?;
			files.push(LayerFile {
				path: relative,
				text: String::from_utf8_lossy(&bytes).into_owned(),
			});
		}
	}
	files.sort_by(|left, right| left.path.cmp(&right.path));
	Ok(ContentLayer {
		replace_paths,
		files,
	})
}

/// `--offline`: interpret the planned cases against the embedded semantics
/// catalog. No game launch, no Steam login. Decisions and history come from
/// the same ordered layers `plan` linted: the matching base when one was
/// loaded, then each mod in load order. Each case folds history at its own date.
fn interpret_report(args: &TestArgs, planned: &Planned) -> HandlerResult {
	let run_id = new_run_id()?;
	let layers = &planned.layers;
	let mut plan = planned.plan.clone();
	plan.decisions =
		overlay_decisions(layers).map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
	let timeline =
		overlay_history(layers).map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
	if !timeline.countries.is_empty() || !timeline.provinces.is_empty() {
		eprintln!(
			"loaded {} country histories and {} province histories from the ordered playset",
			timeline.countries.len(),
			timeline.provinces.len()
		);
	}
	let interpretation = interpret(
		&plan,
		&planned.facts,
		&SemanticsCatalog::embedded(),
		&InterpretOptions {
			run_id: run_id.clone(),
			mod_name: planned.mod_name.clone(),
			observe: !args.no_observe,
			seed: None,
			history: Some(timeline),
			subject_types: Some(
				overlay_subject_types(layers)
					.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?,
			),
		},
	);
	let requested_output = match &args.out {
		Some(output) => output.clone(),
		None => {
			fs::canonicalize(std::env::temp_dir())?.join(format!("foch-tests-offline-{run_id}"))
		}
	};
	let output = prepare_output(&requested_output, &planned.root)?;
	#[derive(Serialize)]
	struct OfflineRunView<'a> {
		source_root: &'a Path,
		output: &'a Path,
		success: bool,
		playset: &'a [String],
		unresolved_dependencies: &'a [String],
		check_scope: &'a CheckScope,
		#[serde(flatten)]
		interpretation: &'a Interpretation,
	}
	let view = OfflineRunView {
		source_root: &planned.root,
		output: &output,
		success: interpretation.exit_code() == 0,
		playset: &planned.load_order,
		unresolved_dependencies: &planned.unresolved,
		check_scope: &planned.check_scope,
		interpretation: &interpretation,
	};
	write_new(
		&output.join("result.json"),
		&serde_json::to_vec_pretty(&view)?,
	)?;
	write_new(
		&output.join("junit.xml"),
		interpretation.to_junit().as_bytes(),
	)?;
	if args.format == CheckOutputFormat::Json {
		print_json(&view)?;
	} else {
		print_scope(planned);
		print_interpreted(&interpretation, &output);
	}
	Ok(interpretation.exit_code())
}

fn print_interpreted(interpretation: &Interpretation, output: &Path) {
	println!(
		"offline layer: modeled semantics only, not game evidence (evidence: {})",
		evidence_label(&interpretation.evidence)
	);
	for case in &interpretation.results {
		println!(
			"{}\t{}\t{}\t{} {}",
			outcome_label(case.outcome),
			case.origin,
			case.node,
			case.start.date,
			case.start.tag,
		);
		for reason in &case.unsupported {
			println!("  {reason}");
		}
		for failure in &case.failures {
			println!("  {}: {}", failure.check, failure.message);
		}
		for note in &case.notes {
			println!("  {note}");
		}
		for observation in &case.trace {
			println!(
				"  trace {} {}  {}",
				observation.date,
				observation.scope,
				observation_label(&observation.event)
			);
		}
	}
	let summary = interpretation.summary();
	println!(
		"{} pass, {} smoke, {} failed, {} setup errors, {} unsupported (offline), {} skipped",
		summary.pass,
		summary.smoke,
		summary.fail,
		summary.setup_error,
		summary.unsupported,
		summary.skipped + summary.ignored,
	);
	println!("Artifacts: {}", output.display());
}

/// One observer-mode trace line, without the leading date and scope.
fn observation_label(observation: &foch_test::Observed) -> String {
	match observation {
		foch_test::Observed::CaseStart => "case start".into(),
		foch_test::Observed::Step { index, kind } => format!("step {index} {kind}"),
		foch_test::Observed::EventFired { event } => format!("fire {event}"),
		foch_test::Observed::EventScheduled { event, due } => format!("schedule {event} for {due}"),
		foch_test::Observed::EventDelivered { event } => format!("deliver {event}"),
		foch_test::Observed::CountryFlagSet { flag } => format!("set flag {flag}"),
		foch_test::Observed::CountryFlagCleared { flag } => format!("clear flag {flag}"),
		foch_test::Observed::GlobalFlagSet { flag } => format!("set global flag {flag}"),
		foch_test::Observed::GlobalFlagCleared { flag } => format!("clear global flag {flag}"),
		foch_test::Observed::VariableSet { which, value } => {
			format!("{which} = {value}")
		}
		foch_test::Observed::SlotSet { slot, value } => format!("{slot} = {value}"),
		foch_test::Observed::WarChanged { with, at_war } => {
			let verb = if *at_war {
				"declare war on"
			} else {
				"peace with"
			};
			format!("{verb} {with}")
		}
		foch_test::Observed::AllianceChanged { with, allied } => {
			let verb = if *allied {
				"ally"
			} else {
				"break alliance with"
			};
			format!("{verb} {with}")
		}
	}
}

fn outcome_label(outcome: Outcome) -> String {
	serde_json::to_value(outcome)
		.ok()
		.and_then(|value| value.as_str().map(str::to_uppercase))
		.unwrap_or_else(|| format!("{outcome:?}"))
}

fn evidence_label(evidence: &foch_test::EvidenceStatus) -> &'static str {
	match evidence {
		foch_test::EvidenceStatus::Unpinned => "unpinned; no decompiled handler batch has run yet",
		foch_test::EvidenceStatus::Pinned => "pinned to decompiled handlers",
	}
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
	load_order: Vec<String>,
	unresolved: Vec<String>,
	check_scope: CheckScope,
	ordered_mods: Vec<DependencyMod>,
	layers: Vec<ContentLayer>,
	facts: ProjectFacts,
}

/// Discover, collect, expand, lint and group. Diagnostics are returned for
/// the caller to print; a plan is always produced so collection can be shown.
/// Tag and event facts come from the same layers the offline interpreter uses.
fn plan(args: &TestArgs, selection: &Selection, shared: bool) -> Result<Planned> {
	let input = &args.path;
	// `PATH::node` selects inside a file, as in pytest.
	let text = input.to_string_lossy();
	let (path, node) = match text.split_once("::") {
		Some((path, node)) => (PathBuf::from(path), Some(node.to_string())),
		None => (input.to_path_buf(), None),
	};
	let (root, files) = discover(&path)?;
	let ordered = resolve_ordered_input(args, &root)?;
	let mod_name = mod_name(&root);
	let mut sources = Vec::new();
	for file in files {
		let relative = file
			.strip_prefix(&root)?
			.to_string_lossy()
			.replace('\\', "/");
		// Files under tests/ are test-only: the game loads events/ but not a
		// top-level tests/ directory, so these carry test and fixture blocks
		// (and tests/events/ helper events) rather than inline annotations.
		let kind = if relative == "tests" || relative.starts_with("tests/") {
			SourceKind::TestsDir
		} else if relative.starts_with("common/scripted_triggers/")
			|| relative.starts_with("common/scripted_effects/")
		{
			SourceKind::Scripted
		} else if relative.starts_with("decisions/") {
			SourceKind::Decisions
		} else {
			SourceKind::Inline
		};
		sources.push(SourceFile {
			mod_name: mod_name.clone(),
			path: RelPath::new(&relative)?,
			kind,
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
			let linted = lint(&expanded, &ordered.facts);
			diagnostics.extend(linted.diagnostics.iter().cloned());
			group(
				expanded,
				&linted,
				&GroupOptions {
					force_isolate: args.isolate,
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
				scripted: Vec::new(),
				decisions: Vec::new(),
			}
		}
	};
	Ok(Planned {
		root,
		mod_name,
		plan,
		diagnostics,
		load_order: ordered.mods.iter().map(|item| item.name.clone()).collect(),
		unresolved: ordered.unresolved,
		check_scope: ordered.check_scope,
		ordered_mods: ordered.mods,
		layers: ordered.layers,
		facts: ordered.facts,
	})
}

/// The mod's declared dependencies, resolved to installed directories, plus
/// the names that could not be resolved.
struct ResolvedDependencies {
	mods: Vec<DependencyMod>,
	unresolved: Vec<String>,
}

/// Resolve the mod under test's declared dependencies (transitively) against
/// the mods installed in the player's user directory. Dependencies are ordered
/// so each appears before the mod that declares it. Resolution needs the user
/// directory's `mod/*.mod` registry; without it, nothing is resolved.
fn resolve_dependencies(source_root: &Path, user_dir: Option<&Path>) -> ResolvedDependencies {
	let mut result = ResolvedDependencies {
		mods: Vec::new(),
		unresolved: Vec::new(),
	};
	let Ok(root_descriptor) = load_descriptor(&source_root.join("descriptor.mod")) else {
		return result;
	};
	if root_descriptor.dependencies.is_empty() {
		return result;
	}
	// name -> directory, from the launcher's installed-mod registry.
	let mut index = BTreeMap::new();
	if let Some(user_dir) = user_dir {
		for entry in fs::read_dir(user_dir.join("mod"))
			.into_iter()
			.flatten()
			.flatten()
		{
			if entry.path().extension().and_then(|v| v.to_str()) != Some("mod") {
				continue;
			}
			if let Ok(descriptor) = load_launcher_descriptor(&entry.path())
				&& let Some(path) = descriptor.path
			{
				index.entry(descriptor.name).or_insert(path);
			}
		}
	}
	let mut visited = std::collections::BTreeSet::new();
	let mut emitted = std::collections::BTreeSet::new();
	visit_dependencies(
		&root_descriptor.dependencies,
		&index,
		&mut visited,
		&mut emitted,
		&mut result,
	);
	result
}

/// Post-order over the dependency graph: a dependency's own dependencies are
/// emitted before it, each mod once, cycles broken by the visited set.
fn visit_dependencies(
	names: &[String],
	index: &BTreeMap<String, PathBuf>,
	visited: &mut std::collections::BTreeSet<String>,
	emitted: &mut std::collections::BTreeSet<String>,
	result: &mut ResolvedDependencies,
) {
	use foch::playset::descriptor::load_descriptor;
	for name in names {
		if !visited.insert(name.clone()) {
			continue;
		}
		let Some(path) = index.get(name) else {
			if !result.unresolved.contains(name) {
				result.unresolved.push(name.clone());
			}
			continue;
		};
		if let Ok(descriptor) = load_descriptor(&path.join("descriptor.mod")) {
			visit_dependencies(&descriptor.dependencies, index, visited, emitted, result);
		}
		if emitted.insert(name.clone()) {
			result.mods.push(DependencyMod {
				name: name.clone(),
				path: path.clone(),
			});
		}
	}
}

/// Install a Ctrl-C handler that sets `cancel`, so the run stops after the
/// current launch is torn down. A second Ctrl-C exits immediately. If a handler
/// is already installed (e.g. the host set one), the run simply has no handler
/// rather than failing.
fn install_cancel_handler(cancel: &Arc<AtomicBool>) {
	let flag = cancel.clone();
	let _ = ctrlc::set_handler(move || {
		if flag.swap(true, Ordering::SeqCst) {
			// Already asked once; the user wants out now.
			std::process::exit(130);
		}
		eprintln!(
			"interrupt received; stopping after the current game is torn down (Ctrl-C again to force)"
		);
	});
}

/// What the output says about tag and event checks.
#[derive(Clone, Debug, Serialize)]
struct CheckScope {
	base: &'static str,
	tags: &'static str,
	events: &'static str,
	detail: &'static str,
}

const SCOPE_MISSING: &str = "Country tags and event definitions were not checked because no matching base was loaded. A vanilla tag or event is not reported as unknown.";
const SCOPE_MATCHING: &str = "Country tags and event definitions were checked from the ordered playset and the matching base. A tag or event in neither is unknown. An event that is defined there but was not collected from the mod under test cannot be checked as hidden and triggered-only.";

impl CheckScope {
	fn missing() -> Self {
		Self {
			base: "missing",
			tags: "not_checked",
			events: "not_checked",
			detail: SCOPE_MISSING,
		}
	}

	fn matching() -> Self {
		Self {
			base: "matching",
			tags: "checked",
			events: "checked",
			detail: SCOPE_MATCHING,
		}
	}
}

/// Static facts for the pre-launch lint, drawn from the active EU4 schema.
/// Effect and trigger names let the linter catch an effect used in a condition
/// block or a trigger used in an effect block before any game runs.
///
/// Scope links are valid in both kinds of block, so they go in both sets and
/// are never flagged. Country tags and event ids are filled only from
/// `definitions`, which the caller parses from the same ordered layers as the
/// offline model and only when a matching base was loaded. Without that base
/// both sets stay unknown, so a vanilla tag is not reported as missing.
fn project_facts(definitions: Option<(BTreeSet<Tag>, BTreeSet<EventId>)>) -> ProjectFacts {
	use foch::game::eu4::{schema_effect_names, schema_link_names, schema_trigger_names};
	let with_links = |names: &[String]| -> BTreeSet<String> {
		names.iter().chain(schema_link_names()).cloned().collect()
	};
	let (tags, events) = match definitions {
		Some((tags, events)) => (Some(tags), Some(events)),
		None => (None, None),
	};
	ProjectFacts {
		tags,
		events,
		effects: Some(with_links(schema_effect_names())),
		triggers: Some(with_links(schema_trigger_names())),
	}
}

struct OrderedInput {
	mods: Vec<DependencyMod>,
	unresolved: Vec<String>,
	layers: Vec<ContentLayer>,
	facts: ProjectFacts,
	check_scope: CheckScope,
}

struct ModRecord {
	load: LoadMod,
	path: PathBuf,
}

/// The ordered mods, layers and facts for this test run.
///
/// `--playset` keeps the author's order and does not open the default EU4
/// user directory. Without it, declared dependencies still resolve through
/// that directory, then the source mod is appended under the runner's
/// existing name.
fn resolve_ordered_input(args: &TestArgs, root: &Path) -> Result<OrderedInput> {
	match &args.playset {
		Some(playset) => resolve_with_playset(args, root, playset),
		None => resolve_without_playset(args, root),
	}
}

fn resolve_without_playset(args: &TestArgs, root: &Path) -> Result<OrderedInput> {
	let user_dir = args.eu4_user_dir.clone().or_else(default_eu4_user_dir);
	let resolved = resolve_dependencies(root, user_dir.as_deref());
	let mut mods = resolved.mods;
	mods.push(DependencyMod {
		name: "Foch source mod".into(),
		path: root.to_path_buf(),
	});
	let base = matching_base(args.game_path.as_deref(), None)?;
	finish_input(mods, resolved.unresolved, base)
}

fn resolve_with_playset(args: &TestArgs, root: &Path, playset: &Path) -> Result<OrderedInput> {
	let (mut author, manifest_game) = load_author_playset(playset)?;
	append_source_if_absent(&mut author, root)?;
	let mut registry = registry_mods(&playset_dir(playset))?;
	if let Some(user_dir) = &args.eu4_user_dir {
		registry.extend(registry_mods(user_dir)?);
	}
	let author_loads: Vec<LoadMod> = author.iter().map(|record| record.load.clone()).collect();
	let registry_loads: Vec<LoadMod> = registry.iter().map(|record| record.load.clone()).collect();
	let (completed, unresolved) = complete_load_order(author_loads, &registry_loads);
	let by_name = index_records(&author, &registry);
	let mut mods = Vec::new();
	for item in &completed {
		let record = by_name
			.get(item.name.as_str())
			.copied()
			.ok_or_else(|| format!("resolved mod {} has no directory", item.name))?;
		mods.push(DependencyMod {
			name: item.name.clone(),
			path: record.path.clone(),
		});
	}
	let base = matching_base(args.game_path.as_deref(), manifest_game.as_deref())?;
	finish_input(mods, unresolved, base)
}

fn finish_input(
	mods: Vec<DependencyMod>,
	unresolved: Vec<String>,
	base: Option<PathBuf>,
) -> Result<OrderedInput> {
	for warning in &unresolved {
		eprintln!("warning: dependency {warning:?} is not installed; testing without it");
	}
	let layers = layers_for(base.as_deref(), &mods)?;
	let (definitions, check_scope) = if base.is_some() {
		let parsed = definition_facts(&layers)
			.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		(Some(parsed), CheckScope::matching())
	} else {
		(None, CheckScope::missing())
	};
	Ok(OrderedInput {
		mods,
		unresolved,
		layers,
		facts: project_facts(definitions),
		check_scope,
	})
}

fn layers_for(base: Option<&Path>, mods: &[DependencyMod]) -> Result<Vec<ContentLayer>> {
	let mut layers = Vec::new();
	if let Some(base) = base {
		layers.push(read_content_layer(base, Vec::new())?);
	}
	for item in mods {
		layers.push(read_content_layer(
			&item.path,
			replace_paths_of(&item.path)?,
		)?);
	}
	Ok(layers)
}

/// A directory named by `--game-path` or the playset's `game_path`. Both
/// directories must be the same path. A path that is not a directory is a
/// missing base, not a Steam lookup.
fn matching_base(
	game_path: Option<&Path>,
	manifest_game_path: Option<&Path>,
) -> Result<Option<PathBuf>> {
	let directory = |path: &Path| path.is_dir().then(|| path.to_path_buf());
	match (
		game_path.and_then(directory),
		manifest_game_path.and_then(directory),
	) {
		(Some(left), Some(right)) => {
			let left = fs::canonicalize(&left)?;
			let right = fs::canonicalize(&right)?;
			if left != right {
				return Err("the --game-path base does not match the playset's game_path".into());
			}
			Ok(Some(left))
		}
		(Some(path), None) | (None, Some(path)) => Ok(Some(fs::canonicalize(&path)?)),
		(None, None) => Ok(None),
	}
}

fn load_author_playset(playset: &Path) -> Result<(Vec<ModRecord>, Option<PathBuf>)> {
	if playset.extension().and_then(|ext| ext.to_str()) == Some("json") {
		return Ok((load_dlc_records(playset)?, None));
	}
	load_manifest_records(playset)
}

fn load_manifest_records(playset: &Path) -> Result<(Vec<ModRecord>, Option<PathBuf>)> {
	let project = Project::load_from_path(playset)?;
	let config = project
		.project
		.ok_or_else(|| format!("{} has no [project] table", playset.display()))?;
	let dir = playset_dir(playset);
	let game_path = config
		.game_path
		.as_ref()
		.map(|path| resolve_path(&dir, path));
	let mut records = Vec::new();
	let mut next_position = 0usize;
	for import in &config.imports {
		match import.kind {
			ProjectImportKind::DlcLoad => {
				let imported = load_dlc_records(&resolve_path(&dir, &import.path))?;
				for mut record in imported {
					record.load.position = next_position;
					next_position += 1;
					records.push(record);
				}
			}
		}
	}
	for manifest_mod in &config.mods {
		if !manifest_mod.enabled {
			continue;
		}
		let position = manifest_mod.position.unwrap_or(next_position);
		next_position += 1;
		let record = record_from_manifest_mod(&dir, manifest_mod, position, playset)?;
		push_replacing(&mut records, record);
	}
	Ok((records, game_path))
}

fn record_from_manifest_mod(
	dir: &Path,
	manifest_mod: &ProjectMod,
	position: usize,
	playset: &Path,
) -> Result<ModRecord> {
	let mut aliases = Vec::new();
	if let Some(id) = manifest_mod
		.id
		.as_deref()
		.map(str::trim)
		.filter(|id| !id.is_empty())
	{
		aliases.push(id.to_string());
	}
	let steam_id = manifest_mod
		.steam_id
		.as_deref()
		.map(str::trim)
		.filter(|id| !id.is_empty());
	if let Some(steam_id) = steam_id {
		aliases.push(steam_id.to_string());
	}
	let root = if let Some(path) = &manifest_mod.path {
		resolve_path(dir, path)
	} else if let Some(steam_id) = steam_id {
		steam_root(dir, steam_id)?
	} else {
		return Err(format!(
			"{}: [[project.mods]] entries require a path or a steam_id",
			playset.display()
		)
		.into());
	};
	record_at(&root, position, aliases)
}

fn steam_root(dir: &Path, steam_id: &str) -> Result<PathBuf> {
	let launcher = dir.join("mod").join(format!("ugc_{steam_id}.mod"));
	if !launcher.is_file() {
		return Err(format!(
			"mod steam_id {steam_id} has no local path; foch test does not look up Steam"
		)
		.into());
	}
	let descriptor = load_launcher_descriptor(&launcher)?;
	let Some(path) = descriptor.path else {
		return Err(format!(
			"mod steam_id {steam_id} has no local path; foch test does not look up Steam"
		)
		.into());
	};
	let path = if path.is_absolute() {
		path
	} else {
		dir.join(path)
	};
	if !path.is_dir() {
		return Err(format!(
			"mod steam_id {steam_id} has no local path; foch test does not look up Steam"
		)
		.into());
	}
	Ok(path)
}

#[derive(serde::Deserialize)]
struct EnabledModsFile {
	#[serde(default)]
	enabled_mods: Vec<String>,
}

fn load_dlc_records(path: &Path) -> Result<Vec<ModRecord>> {
	foch::playset::Playset::from_dlc_load(path)?;
	let bytes = fs::read(path)?;
	let file: EnabledModsFile = serde_json::from_slice(&bytes)?;
	let parent = playset_dir(path);
	let mut records = Vec::new();
	for (position, rel) in file.enabled_mods.iter().enumerate() {
		let descriptor_path = rel
			.split('/')
			.filter(|part| !part.is_empty())
			.fold(parent.to_path_buf(), |path, part| path.join(part));
		let launcher = load_launcher_descriptor(&descriptor_path)?;
		let Some(mod_path) = launcher.path else {
			return Err(format!("playset entry {rel} has no local mod directory").into());
		};
		let mod_path = if mod_path.is_absolute() {
			mod_path
		} else {
			parent.join(mod_path)
		};
		let mut aliases = Vec::new();
		if let Some(id) = launcher.remote_file_id.filter(|id| !id.trim().is_empty()) {
			aliases.push(id);
		}
		if !launcher.name.trim().is_empty() {
			aliases.push(launcher.name);
		}
		records.push(record_at(&mod_path, position, aliases)?);
	}
	Ok(records)
}

fn registry_mods(dir: &Path) -> Result<Vec<ModRecord>> {
	let mut records = Vec::new();
	let entries = match fs::read_dir(dir.join("mod")) {
		Ok(entries) => entries,
		Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(records),
		Err(err) => return Err(err.into()),
	};
	for entry in entries.flatten() {
		if entry.path().extension().and_then(|ext| ext.to_str()) != Some("mod") {
			continue;
		}
		let Ok(launcher) = load_launcher_descriptor(&entry.path()) else {
			continue;
		};
		let Some(path) = launcher.path else {
			continue;
		};
		let mut aliases = Vec::new();
		if let Some(id) = launcher.remote_file_id.filter(|id| !id.trim().is_empty()) {
			aliases.push(id);
		}
		let Some(mut record) = try_record(&path, 0, aliases)? else {
			continue;
		};
		if !launcher.name.trim().is_empty() && launcher.name != record.load.name {
			record.load.aliases.push(launcher.name);
		}
		records.push(record);
	}
	Ok(records)
}

fn try_record(path: &Path, position: usize, aliases: Vec<String>) -> Result<Option<ModRecord>> {
	if !path.is_dir() {
		return Ok(None);
	}
	Ok(Some(record_at(path, position, aliases)?))
}

fn record_at(path: &Path, position: usize, mut aliases: Vec<String>) -> Result<ModRecord> {
	if !path.is_dir() {
		return Err(format!("mod directory does not exist: {}", path.display()).into());
	}
	let descriptor = if path.join("descriptor.mod").is_file() {
		Some(load_descriptor(&path.join("descriptor.mod"))?)
	} else {
		None
	};
	let name = descriptor
		.as_ref()
		.map(|descriptor| descriptor.name.trim())
		.filter(|name| !name.is_empty() && !name.contains("::"))
		.map(str::to_string)
		.or_else(|| {
			path.file_name()
				.map(|name| name.to_string_lossy().into_owned())
		})
		.unwrap_or_else(|| "mod".into());
	if let Some(id) = descriptor
		.as_ref()
		.and_then(|descriptor| descriptor.remote_file_id.clone())
		.filter(|id| !id.trim().is_empty() && id != &name && !aliases.contains(id))
	{
		aliases.push(id);
	}
	aliases.retain(|alias| alias != &name);
	let dependencies = descriptor
		.map(|descriptor| descriptor.dependencies)
		.unwrap_or_default();
	Ok(ModRecord {
		load: LoadMod {
			name,
			aliases,
			dependencies,
			position,
		},
		path: path.to_path_buf(),
	})
}

fn append_source_if_absent(author: &mut Vec<ModRecord>, root: &Path) -> Result<()> {
	if author.iter().any(|record| same_dir(&record.path, root)) {
		return Ok(());
	}
	let position = author
		.iter()
		.map(|record| record.load.position)
		.max()
		.map(|position| position + 1)
		.unwrap_or(0);
	author.push(record_at(root, position, Vec::new())?);
	Ok(())
}

fn same_dir(left: &Path, right: &Path) -> bool {
	match (fs::canonicalize(left), fs::canonicalize(right)) {
		(Ok(left), Ok(right)) => left == right,
		_ => left == right,
	}
}

fn index_records<'a>(
	author: &'a [ModRecord],
	registry: &'a [ModRecord],
) -> BTreeMap<&'a str, &'a ModRecord> {
	let mut map = BTreeMap::new();
	for record in registry {
		map.entry(record.load.name.as_str()).or_insert(record);
	}
	for record in author {
		map.insert(record.load.name.as_str(), record);
	}
	map
}

fn push_replacing(records: &mut Vec<ModRecord>, record: ModRecord) {
	let path = fs::canonicalize(&record.path).unwrap_or_else(|_| record.path.clone());
	let mut tokens = BTreeSet::new();
	tokens.insert(record.load.name.clone());
	tokens.extend(record.load.aliases.iter().cloned());
	records.retain(|existing| {
		let existing_path =
			fs::canonicalize(&existing.path).unwrap_or_else(|_| existing.path.clone());
		if existing_path == path || tokens.contains(&existing.load.name) {
			return false;
		}
		!existing
			.load
			.aliases
			.iter()
			.any(|alias| tokens.contains(alias))
	});
	records.push(record);
}

fn playset_dir(playset: &Path) -> PathBuf {
	fs::canonicalize(playset)
		.ok()
		.and_then(|path| path.parent().map(Path::to_path_buf))
		.filter(|path| !path.as_os_str().is_empty())
		.unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_path(dir: &Path, path: &Path) -> PathBuf {
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		dir.join(path)
	}
}

fn print_scope(planned: &Planned) {
	let scope = &planned.check_scope;
	println!(
		"check scope: base {}, tags {}, events {}",
		scope.base, scope.tags, scope.events
	);
	println!("{}", scope.detail);
	if !planned.load_order.is_empty() {
		println!("playset: {}", planned.load_order.join(", "));
	}
	if !planned.unresolved.is_empty() {
		println!("unresolved dependencies: {}", planned.unresolved.join(", "));
	}
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
		files.extend(tests_files(&root)?);
		files.extend(scripted_files(&root)?);
		files.extend(directory_files(&root, "decisions")?);
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
	let mut files = event_files(&root)?;
	files.extend(tests_files(&root)?);
	files.extend(scripted_files(&root)?);
	files.extend(directory_files(&root, "decisions")?);
	Ok((root, files))
}

/// `.txt` files under the mod's scripted definition directories. The game
/// loads these, so they carry plain top-level `name = { ... }` definitions.
fn scripted_files(root: &Path) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();
	for name in ["common/scripted_triggers", "common/scripted_effects"] {
		files.extend(directory_files(root, name)?);
	}
	Ok(files)
}

/// `.txt` files below one mod content directory, refusing symlinks.
fn directory_files(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();
	{
		let directory = root.join(name);
		match fs::symlink_metadata(&directory) {
			Ok(metadata) if metadata.file_type().is_symlink() => {
				return Err(format!("{name} directory must not be a symlink").into());
			}
			Ok(_) => {
				for entry in WalkDir::new(directory).follow_links(false) {
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
	}
	files.sort();
	Ok(files)
}

/// `.txt` files under the mod's `tests/` directory. The game does not load
/// this directory, so these hold test and fixture blocks (and, under
/// `tests/events/`, helper events for the generated test layer only).
fn tests_files(root: &Path) -> Result<Vec<PathBuf>> {
	let tests = root.join("tests");
	let mut files = Vec::new();
	match fs::symlink_metadata(&tests) {
		Ok(metadata) if metadata.file_type().is_symlink() => {
			return Err("tests directory must not be a symlink".into());
		}
		Ok(_) => {
			for entry in WalkDir::new(tests).follow_links(false) {
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
			"playset": planned.load_order,
			"unresolved_dependencies": planned.unresolved,
			"check_scope": planned.check_scope,
		}))?;
	} else {
		print_scope(planned);
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
	playset: &'a [String],
	unresolved_dependencies: &'a [String],
	check_scope: &'a CheckScope,
	#[serde(flatten)]
	report: &'a Report,
}

/// What the CLI observed about one game launch.
#[derive(Clone, Debug, Serialize)]
struct SessionRecord {
	cases: Vec<String>,
	launch: String,
	/// The plugin host's events file, when plugins were loaded.
	host_events: Option<PathBuf>,
	/// The OS process id of the launched game, captured at spawn.
	launch_pid: u32,
	/// The Windows session the game ran in; `None` when unavailable.
	launch_session_id: Option<u32>,
	#[serde(flatten)]
	exit: foch_test::RunnerExit,
}

/// Build one session's test layer, launch the game through `foch-runner`,
/// judge the log, and record what was launched.
#[allow(clippy::too_many_arguments)]
fn run_one_session(
	plan: &Plan,
	bundle: &Bundle,
	source_root: &Path,
	ordered_mods: &[DependencyMod],
	directory: &Path,
	installation: &Installation,
	options: &RunOptions,
) -> Result<(SessionRecord, Vec<CaseResult>)> {
	// Write the full bundle (test layer, commands, manifest) for inspection;
	// the runner reuses the test layer it leaves in `directory`.
	materialize(bundle, source_root, directory)?;
	let outcome =
		foch_runtime::runner::run_session(bundle, ordered_mods, directory, installation, options)?;
	// The runner preserves the plugin host's event stream beside the
	// session's logs; receipts are judged from that copy, against the
	// launch identity the runner actually stamped into the host plan.
	let host_events = outcome
		.host_events
		.as_deref()
		.and_then(|path| std::fs::read_to_string(path).ok());
	let results = judge(
		bundle,
		plan,
		&RunArtifacts {
			game_log: outcome.game_log.as_deref(),
			error_log: outcome.error_log.as_deref(),
			exit: outcome.exit.clone(),
			timing: outcome.timing,
			profile_untouched: outcome.profile_untouched,
			host_events: host_events.as_deref(),
			host_launch_id: outcome.host_launch_id.as_deref(),
		},
	);
	let record = SessionRecord {
		cases: bundle
			.cases
			.iter()
			.map(|&index| plan.case(index).case.node.to_string())
			.collect(),
		launch: outcome.launch_detail,
		host_events: outcome.host_events,
		launch_pid: outcome.launch_pid,
		launch_session_id: outcome.launch_session_id,
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

#[cfg(test)]
mod tests {
	use super::*;

	fn write(path: &Path, text: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, text).unwrap();
	}

	fn forward(path: &Path) -> String {
		path.display().to_string().replace('\\', "/")
	}

	#[test]
	fn dependencies_resolve_transitively_and_report_missing() {
		let temp = tempfile::tempdir().unwrap();
		let base = temp.path();
		// Installed mods: Dep A depends on Dep B; Dep B stands alone.
		let dep_a = base.join("installed/a");
		let dep_b = base.join("installed/b");
		write(
			&dep_a.join("descriptor.mod"),
			"name=\"Dep A\"\ndependencies={ \"Dep B\" }\n",
		);
		write(&dep_b.join("descriptor.mod"), "name=\"Dep B\"\n");
		// The launcher registry under the user directory names both.
		let user_dir = base.join("user");
		write(
			&user_dir.join("mod/a.mod"),
			&format!("name=\"Dep A\"\npath=\"{}\"\n", forward(&dep_a)),
		);
		write(
			&user_dir.join("mod/b.mod"),
			&format!("name=\"Dep B\"\npath=\"{}\"\n", forward(&dep_b)),
		);
		// The mod under test depends on Dep A and a mod that is not installed.
		let source = base.join("source");
		write(
			&source.join("descriptor.mod"),
			"name=\"Src\"\ndependencies={ \"Dep A\" \"Ghost\" }\n",
		);

		let resolved = resolve_dependencies(&source, Some(&user_dir));
		let order: Vec<_> = resolved.mods.iter().map(|m| m.name.as_str()).collect();
		assert_eq!(
			order,
			["Dep B", "Dep A"],
			"a dependency precedes its dependent"
		);
		assert_eq!(resolved.unresolved, ["Ghost"]);
	}

	#[test]
	fn a_mod_without_dependencies_resolves_to_nothing() {
		let temp = tempfile::tempdir().unwrap();
		let source = temp.path().join("source");
		write(&source.join("descriptor.mod"), "name=\"Src\"\n");
		let resolved = resolve_dependencies(&source, None);
		assert!(resolved.mods.is_empty() && resolved.unresolved.is_empty());
	}
}
