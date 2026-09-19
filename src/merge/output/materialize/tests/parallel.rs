//! Materialization with several workers: the output matches one worker, units
//! overlap within the worker limit, and definition modules stay whole.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use super::super::{
	MaterializeOutput, MaterializedMerge, MergeMaterializeOptions, StructuralConflictReport,
	StructuralMergeFailure, StructuralMergeOutput, freeze_path_plan, materialize_analyzed_input,
};
use super::{
	idea_file, no_base_options, request_for, structural_merge_output, write_descriptor,
	write_descriptor_with_dependencies, write_dlc_load, write_file,
};
use crate::input::request::InputRequest;
use crate::input::{InputResolveError, ResolvedInput, resolve_input};
use crate::merge::analyze::CancellationToken;
use crate::merge::backend::{
	AddressPatchBackend, BackendOutcome, BackendProfile, BackendRequest, BackendUnit, MergeBackend,
	MergeBackendDescriptor, MergeBackendId, backend_for,
};
use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};
use crate::merge::conflict_view::ConflictView;
use crate::merge::model::ExternalFileResolution;
use crate::merge::{MergeDisposition, MergeError, MergeReviewSummary, MergeUnitOutcome};
use crate::model::{
	HandlerResolutionRecord, MERGE_REPORT_ARTIFACT_PATH, MergePlanEntry, MergePlanResult,
	MergePlanStrategy, MergeReport, MergeReportStatus, StaleVanillaTargetDescriptor,
};
use crate::project::{ResolutionDecision, ResolutionMap};

/// Turns a hang into a test failure. Nothing waits this long when the
/// executor behaves.
const DEADLOCK_GUARD: Duration = Duration::from_secs(30);
/// How long a test keeps units busy so that a worker beyond the limit, if one
/// existed, would start another unit. A correct executor passes however short
/// this is; only a broken one can slip through a shorter hold.
const EXTRA_WORKER_WINDOW: Duration = Duration::from_millis(100);
const WORKER_THREAD_PREFIX: &str = "foch-merge-";
const STATIC_MODIFIERS_INPUT: &str = "common/static_modifiers/par_a.txt";
const EVENT_MODIFIERS_INPUT: &str = "common/event_modifiers/par_b.txt";
/// Sibling conflicts in plan order. The first and last bracket the others, so
/// holding the first until the last has finished reverses their completion.
const EARLY_CONFLICT_PATH: &str = "history/countries/AAA - Early.txt";
const GENUINE_CONFLICT_PATH: &str = "history/countries/GEN - Genuine.txt";
/// Settled by a configured decision for the file.
const CHOSEN_CONFLICT_PATH: &str = "history/countries/MID - Chosen.txt";
/// Settled by the mod that depends on both siblings.
const DOWNSTREAM_RESOLVED_PATH: &str = "history/countries/RES - Resolved.txt";
const LATE_CONFLICT_PATH: &str = "history/countries/ZZZ - Late.txt";
/// Unparseable in one mod, so the plan defers it; it sits between file units
/// that workers analyze.
const BROKEN_PATH: &str = "history/provinces/p03_broken.txt";
const LOCALISATION_PATH: &str = "localisation/par_l_english.yml";
const OVERLAY_PATH: &str = "gfx/interface/par_icon.dds";
const SAFE_FILE_COUNT: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn workers(count: usize) -> NonZeroUsize {
	NonZeroUsize::new(count).expect("at least one worker")
}

fn province_path(index: usize) -> String {
	format!("history/provinces/p{index:02}.txt")
}

fn province_index(path: &str) -> usize {
	path.strip_prefix("history/provinces/p")
		.and_then(|rest| rest.strip_suffix(".txt"))
		.and_then(|index| index.parse().ok())
		.unwrap_or_else(|| panic!("{path} is not a numbered province"))
}

fn plan_position(order: &[String], path: &str) -> usize {
	order
		.iter()
		.position(|entry| entry == path)
		.unwrap_or_else(|| panic!("plan has no unit for {path}"))
}

/// A playset resolved and planned once. Every run materializes the same plan
/// into its own artifacts directory, against one shared target directory.
struct FrozenPlayset {
	root: PathBuf,
	request: InputRequest,
	input: Result<ResolvedInput, InputResolveError>,
	plan: MergePlanResult,
}

impl FrozenPlayset {
	fn freeze(root: &Path) -> Self {
		let request: InputRequest = request_for(&root.join("playlist.json"));
		let mut input: Result<ResolvedInput, InputResolveError> = resolve_input(&request, false);
		let plan: MergePlanResult = freeze_path_plan(&mut input, false, &ResolutionMap::default());
		assert!(!plan.has_fatal_errors(), "{:?}", plan.fatal_errors);
		Self {
			root: root.to_path_buf(),
			request,
			input,
			plan,
		}
	}

	/// The input resolved again. Its scripts are parsed on demand, so a run
	/// with several workers parses on its workers rather than reusing what an
	/// earlier run of the same test already parsed.
	fn fresh_input(&self) -> Result<ResolvedInput, InputResolveError> {
		let mut input: Result<ResolvedInput, InputResolveError> =
			resolve_input(&self.request, false);
		let plan: MergePlanResult = freeze_path_plan(&mut input, false, &ResolutionMap::default());
		assert_eq!(
			serde_json::to_value(&plan.paths).unwrap(),
			serde_json::to_value(&self.plan.paths).unwrap(),
			"the input no longer freezes to the same plan"
		);
		input
	}

	fn count(&self, strategy: MergePlanStrategy) -> usize {
		self.plan
			.paths
			.iter()
			.filter(|entry| entry.strategy == strategy)
			.count()
	}

	fn entry(&self, path: &str) -> &MergePlanEntry {
		self.plan
			.paths
			.iter()
			.find(|entry| entry.output_path() == path)
			.unwrap_or_else(|| panic!("plan has no unit for {path}"))
	}

	fn module(&self) -> &MergePlanEntry {
		let modules: Vec<&MergePlanEntry> = self
			.plan
			.paths
			.iter()
			.filter(|entry| !entry.target.module_outputs().is_empty())
			.collect();
		assert_eq!(modules.len(), 1, "the fixture plans one definition module");
		modules[0]
	}

	fn output_paths(&self) -> Vec<String> {
		self.plan
			.paths
			.iter()
			.map(|entry| entry.output_path().to_string())
			.collect()
	}

	fn materialize(
		&self,
		run: &str,
		options: MergeMaterializeOptions,
	) -> (Result<MaterializedMerge, MergeError>, PathBuf) {
		self.materialize_input(run, options, self.fresh_input())
	}

	/// Materialize the input exactly as it was frozen, even if a source file
	/// changed since.
	fn materialize_frozen(
		&self,
		run: &str,
		options: MergeMaterializeOptions,
	) -> (Result<MaterializedMerge, MergeError>, PathBuf) {
		self.materialize_input(run, options, self.input.clone())
	}

	fn materialize_input(
		&self,
		run: &str,
		options: MergeMaterializeOptions,
		input: Result<ResolvedInput, InputResolveError>,
	) -> (Result<MaterializedMerge, MergeError>, PathBuf) {
		let artifacts_dir: PathBuf = self.root.join("runs").join(run);
		let target_dir: PathBuf = self.root.join("target");
		let result: Result<MaterializedMerge, MergeError> = materialize_analyzed_input(
			self.request.clone(),
			MaterializeOutput {
				artifacts_dir: &artifacts_dir,
				prior_dir: None,
				target_dir: &target_dir,
			},
			options,
			input,
			self.plan.clone(),
			None,
		);
		(result, artifacts_dir)
	}
}

/// Two mods, each adding its own keys to `count` province history files: one
/// safe structural file unit per file.
fn write_province_playset(root: &Path, count: usize) {
	write_dlc_load(
		&root.join("playlist.json"),
		&[("par-a", "A"), ("par-b", "B")],
	);
	for (mod_id, key) in [("par-a", "from_a"), ("par-b", "from_b")] {
		let mod_root: PathBuf = root.join(mod_id);
		write_descriptor(&mod_root, mod_id);
		for index in 0..count {
			write_file(
				&mod_root,
				&province_path(index),
				format!("{key}_{index} = yes\n"),
			);
		}
	}
}

/// One EU4 database fed by two directories: a single module unit with two
/// outputs, next to `file_count` structural file units.
fn write_database_playset(root: &Path, file_count: usize) {
	write_province_playset(root, file_count);
	write_file(
		&root.join("par-a"),
		STATIC_MODIFIERS_INPUT,
		"par_from_a = { tax_income = 1 }\n",
	);
	write_file(
		&root.join("par-b"),
		EVENT_MODIFIERS_INPUT,
		"par_from_b = { tax_income = 2 }\n",
	);
	write_file(root, "eu4-game/version.txt", "1.37.5\n");
}

/// Every kind of unit in one playset: a base mod, two siblings that depend on
/// it, and a downstream mod that depends on both siblings.
fn write_mixed_playset(root: &Path) {
	write_dlc_load(
		&root.join("playlist.json"),
		&[
			("par-base", "Base"),
			("par-a", "A"),
			("par-b", "B"),
			("par-c", "C"),
		],
	);
	let [base, a, b, c]: [PathBuf; 4] =
		["par-base", "par-a", "par-b", "par-c"].map(|mod_id| root.join(mod_id));
	write_descriptor(&base, "par-base");
	write_descriptor_with_dependencies(&a, "par-a", &["par-base"]);
	write_descriptor_with_dependencies(&b, "par-b", &["par-base"]);
	write_descriptor_with_dependencies(&c, "par-c", &["par-a", "par-b"]);
	// Independent structural merges: every mod adds its own key.
	for index in 0..SAFE_FILE_COUNT {
		let path: String = province_path(index);
		write_file(&a, &path, format!("from_a_{index} = yes\n"));
		write_file(&b, &path, format!("from_b_{index} = yes\n"));
		if index % 2 == 1 {
			write_file(&c, &path, format!("from_c_{index} = yes\n"));
		}
	}
	// Siblings rewrite the same base value. Nothing downstream settles these,
	// except the configured decision for the chosen one.
	for path in [
		EARLY_CONFLICT_PATH,
		GENUINE_CONFLICT_PATH,
		CHOSEN_CONFLICT_PATH,
		DOWNSTREAM_RESOLVED_PATH,
		LATE_CONFLICT_PATH,
	] {
		write_file(&base, path, idea_file("old"));
		write_file(&a, path, idea_file("alpha"));
		write_file(&b, path, idea_file("beta"));
	}
	// The mod that depends on both siblings settles this one.
	write_file(&c, DOWNSTREAM_RESOLVED_PATH, idea_file("gamma"));
	write_file(&a, BROKEN_PATH, "= broken\n");
	write_file(&b, BROKEN_PATH, "fine = yes\n");
	write_file(
		&a,
		STATIC_MODIFIERS_INPUT,
		"par_from_a = { tax_income = 1 }\n",
	);
	write_file(
		&b,
		EVENT_MODIFIERS_INPUT,
		"par_from_b = { tax_income = 2 }\n",
	);
	write_file(root, "eu4-game/version.txt", "1.37.5\n");
	write_file(
		&a,
		LOCALISATION_PATH,
		"l_english:\n par_loc_a:0 \"From A\"\n",
	);
	write_file(
		&b,
		LOCALISATION_PATH,
		"l_english:\n par_loc_b:0 \"From B\"\n",
	);
	write_file(&a, OVERLAY_PATH, [1u8, 2, 3]);
	write_file(&b, OVERLAY_PATH, [4u8, 5, 6]);
	write_file(&a, "gfx/interface/only_a.dds", [7u8, 8, 9]);
	write_file(&c, "history/provinces/only_c.txt", "owner = SWE\n");
}

/// The configured decision that settles the chosen conflict in mod A's favour.
fn chosen_resolution() -> ResolutionMap {
	let mut resolution_map: ResolutionMap = ResolutionMap::default();
	resolution_map.by_file.insert(
		PathBuf::from(CHOSEN_CONFLICT_PATH),
		ResolutionDecision::PreferMod("par-a".to_string()),
	);
	resolution_map
}

/// Everything a run produced that must not depend on the number of workers.
struct RunSnapshot {
	tree: BTreeMap<String, Vec<u8>>,
	report: Value,
	units: Vec<MergeUnitOutcome>,
	summary: MergeReviewSummary,
}

impl RunSnapshot {
	fn capture(merged: &MaterializedMerge, artifacts_dir: &Path) -> Self {
		Self {
			tree: output_tree(artifacts_dir),
			report: comparable_report(&merged.report),
			units: merged.review.units().to_vec(),
			summary: *merged.review.summary(),
		}
	}

	fn assert_matches(&self, other: &Self, run: &str) {
		let paths: Vec<&String> = self.tree.keys().collect();
		let other_paths: Vec<&String> = other.tree.keys().collect();
		assert_eq!(paths, other_paths, "{run}: output paths differ");
		for (path, bytes) in &self.tree {
			assert!(
				other.tree[path] == *bytes,
				"{run}: {path} differs\nexpected:\n{}\nactual:\n{}",
				String::from_utf8_lossy(bytes),
				String::from_utf8_lossy(&other.tree[path])
			);
		}
		assert_eq!(self.report, other.report, "{run}: report differs");
		assert_eq!(self.units, other.units, "{run}: review units differ");
		assert_eq!(self.summary, other.summary, "{run}: review summary differs");
	}
}

/// Every file and directory below `root`, by relative path; directories end
/// in `/` so a leftover empty directory is a difference too. The report
/// artifact is kept without its wall-clock field.
fn output_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
	let mut tree: BTreeMap<String, Vec<u8>> = BTreeMap::new();
	let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
	while let Some(directory) = pending.pop() {
		for entry in fs::read_dir(&directory).expect("read output directory") {
			let path: PathBuf = entry.expect("read output entry").path();
			let relative: String = path
				.strip_prefix(root)
				.expect("entry below root")
				.to_string_lossy()
				.replace('\\', "/");
			if path.is_dir() {
				tree.insert(format!("{relative}/"), Vec::new());
				pending.push(path);
				continue;
			}
			let mut bytes: Vec<u8> = fs::read(&path).expect("read output file");
			if relative == MERGE_REPORT_ARTIFACT_PATH {
				let report: Value = serde_json::from_slice(&bytes).expect("parse report artifact");
				bytes = serde_json::to_vec(&without_wall_clock(report)).expect("serialize report");
			}
			tree.insert(relative, bytes);
		}
	}
	tree
}

fn comparable_report(report: &MergeReport) -> Value {
	without_wall_clock(serde_json::to_value(report).expect("serialize report"))
}

/// Module time is wall-clock, so it is the one report field that may differ.
fn without_wall_clock(mut report: Value) -> Value {
	let removed: Option<Value> = report
		.as_object_mut()
		.expect("the report is a JSON object")
		.remove("definition_module_elapsed_ms");
	assert!(removed.is_some(), "the report records module time");
	report
}

/// Targets whose backend call has returned. A wait that outlasts the deadlock
/// guard is recorded instead of hanging the test, and every later wait then
/// returns at once so a broken executor fails quickly.
#[derive(Default)]
struct Exits {
	exited: Mutex<BTreeSet<String>>,
	changed: Condvar,
	timed_out: AtomicBool,
}

impl Exits {
	fn mark(&self, target: &str) {
		lock(&self.exited).insert(target.to_string());
		self.changed.notify_all();
	}

	fn wait_for(&self, targets: &[&str]) {
		if self.timed_out() {
			return;
		}
		let (exited, result) = self
			.changed
			.wait_timeout_while(lock(&self.exited), DEADLOCK_GUARD, |exited| {
				!targets.iter().all(|target| exited.contains(*target))
			})
			.unwrap_or_else(PoisonError::into_inner);
		drop(exited);
		if result.timed_out() {
			self.timed_out.store(true, Ordering::SeqCst);
		}
	}

	fn timed_out(&self) -> bool {
		self.timed_out.load(Ordering::SeqCst)
	}
}

/// One backend call as the test observed it.
#[derive(Clone, Debug)]
struct Call {
	target: String,
	module: bool,
	thread: ThreadId,
	thread_name: String,
	/// Positions on one clock shared by every call, taken on entry and exit.
	entered: usize,
	exited: usize,
}

impl Call {
	fn on_worker(&self) -> bool {
		self.thread_name.starts_with(WORKER_THREAD_PREFIX)
	}
}

#[derive(Default)]
struct Recorder {
	clock: AtomicUsize,
	started: AtomicUsize,
	active: AtomicUsize,
	max_active: AtomicUsize,
	calls: Mutex<Vec<Call>>,
	exits: Exits,
}

impl Recorder {
	fn started(&self) -> usize {
		self.started.load(Ordering::SeqCst)
	}

	fn active(&self) -> usize {
		self.active.load(Ordering::SeqCst)
	}

	fn max_active(&self) -> usize {
		self.max_active.load(Ordering::SeqCst)
	}

	fn calls(&self) -> Vec<Call> {
		lock(&self.calls).clone()
	}

	fn call(&self, target: &str) -> Call {
		self.calls()
			.into_iter()
			.find(|call| call.target == target)
			.unwrap_or_else(|| panic!("no backend call for {target}"))
	}
}

/// What a hook sees of one backend call.
struct Attempt<'a> {
	target: &'a str,
	module: bool,
	/// 1-based position among every call.
	position: usize,
}

/// Runs around every call. `proceed` runs the wrapped backend; returning
/// without calling it replaces the wrapped backend's outcome.
type Hook = Box<
	dyn Fn(&Attempt<'_>, &Recorder, &mut dyn FnMut() -> BackendOutcome) -> BackendOutcome
		+ Send
		+ Sync,
>;

/// Wraps a backend and records every call.
struct RecordingBackend {
	inner: Box<dyn MergeBackend>,
	recorder: Arc<Recorder>,
	hook: Hook,
}

impl MergeBackend for RecordingBackend {
	fn descriptor(&self) -> MergeBackendDescriptor {
		self.inner.descriptor()
	}

	fn profile(&self) -> BackendProfile {
		self.inner.profile()
	}

	fn analyze(&self, request: BackendRequest<'_, '_>) -> BackendOutcome {
		let recorder: &Recorder = &self.recorder;
		let entered: usize = recorder.clock.fetch_add(1, Ordering::SeqCst);
		let position: usize = recorder.started.fetch_add(1, Ordering::SeqCst) + 1;
		let active: usize = recorder.active.fetch_add(1, Ordering::SeqCst) + 1;
		recorder.max_active.fetch_max(active, Ordering::SeqCst);
		let target: String = request.target_path.to_string();
		let module: bool = matches!(request.unit, BackendUnit::DefinitionModule(_));
		let attempt: Attempt<'_> = Attempt {
			target: &target,
			module,
			position,
		};
		let inner: &dyn MergeBackend = &*self.inner;
		let mut request: Option<BackendRequest<'_, '_>> = Some(request);
		let mut proceed = || inner.analyze(request.take().expect("a hook proceeds at most once"));
		let outcome: BackendOutcome = (self.hook)(&attempt, recorder, &mut proceed);
		recorder.active.fetch_sub(1, Ordering::SeqCst);
		let exited: usize = recorder.clock.fetch_add(1, Ordering::SeqCst);
		let current: thread::Thread = thread::current();
		lock(&recorder.calls).push(Call {
			target: target.clone(),
			module,
			thread: current.id(),
			thread_name: current.name().unwrap_or_default().to_string(),
			entered,
			exited,
		});
		recorder.exits.mark(&target);
		outcome
	}
}

fn recording_over(
	inner: Box<dyn MergeBackend>,
	hook: impl Fn(&Attempt<'_>, &Recorder, &mut dyn FnMut() -> BackendOutcome) -> BackendOutcome
	+ Send
	+ Sync
	+ 'static,
) -> (Box<dyn MergeBackend>, Arc<Recorder>) {
	let recorder: Arc<Recorder> = Arc::default();
	let backend: Box<dyn MergeBackend> = Box::new(RecordingBackend {
		inner,
		recorder: Arc::clone(&recorder),
		hook: Box::new(hook),
	});
	(backend, recorder)
}

/// Records calls to the address-patch backend.
fn recording(
	hook: impl Fn(&Attempt<'_>, &Recorder, &mut dyn FnMut() -> BackendOutcome) -> BackendOutcome
	+ Send
	+ Sync
	+ 'static,
) -> (Box<dyn MergeBackend>, Arc<Recorder>) {
	recording_over(Box::new(AddressPatchBackend), hook)
}

fn options_with(backend: Box<dyn MergeBackend>, worker_count: usize) -> MergeMaterializeOptions {
	let mut options: MergeMaterializeOptions = no_base_options(false);
	options.backend = backend;
	options.workers = workers(worker_count);
	options
}

/// A merge whose output names a frozen external payload that was never
/// prepared, so applying it fails the whole merge.
fn missing_payload_output(payload: &str, target: &str) -> BackendOutcome {
	let mut output: StructuralMergeOutput = structural_merge_output("unused = yes\n");
	output.external_file_resolutions.insert(
		PathBuf::from(target),
		ExternalFileResolution::Frozen(PathBuf::from(payload)),
	);
	Ok(output)
}

fn unit_for<'a>(merged: &'a MaterializedMerge, path: &str) -> &'a MergeUnitOutcome {
	merged
		.review
		.units()
		.iter()
		.find(|unit| unit.path == path)
		.unwrap_or_else(|| panic!("review has no unit for {path}"))
}

/// Where the first warning naming `path` sits, if any.
fn warning_position(report: &MergeReport, path: &str) -> usize {
	report
		.warnings
		.iter()
		.position(|warning| warning.contains(path))
		.unwrap_or_else(|| panic!("no warning names {path}: {:?}", report.warnings))
}

/// The fixture must reach every kind of unit and put several entries into
/// each report list whose order follows the order units are applied in, or
/// matching one worker proves little.
fn assert_mixed_run_is_not_vacuous(
	backend: MergeBackendId,
	frozen: &FrozenPlayset,
	merged: &MaterializedMerge,
	artifacts_dir: &Path,
	force: bool,
	provenance: bool,
	run: &str,
) {
	let report: &MergeReport = &merged.report;
	assert_eq!(report.status, MergeReportStatus::PartialSuccess, "{run}");
	for index in 0..SAFE_FILE_COUNT {
		let path: String = province_path(index);
		assert_eq!(
			unit_for(merged, &path).disposition,
			MergeDisposition::Safe,
			"{run}: {path}"
		);
	}
	for path in [
		EARLY_CONFLICT_PATH,
		GENUINE_CONFLICT_PATH,
		LATE_CONFLICT_PATH,
	] {
		let conflict: &MergeUnitOutcome = unit_for(merged, path);
		assert_eq!(
			conflict.disposition,
			MergeDisposition::NeedsUserChoice,
			"{run}: {conflict:?}"
		);
		// With --force a conflict writes a placeholder; without it, nothing.
		assert_eq!(
			conflict.output_path.as_deref(),
			force.then_some(path),
			"{run}"
		);
		assert_eq!(artifacts_dir.join(path).is_file(), force, "{run}: {path}");
	}
	for (path, winner) in [
		(CHOSEN_CONFLICT_PATH, "alpha"),
		(DOWNSTREAM_RESOLVED_PATH, "gamma"),
	] {
		let settled: &MergeUnitOutcome = unit_for(merged, path);
		assert_eq!(
			settled.disposition,
			MergeDisposition::Safe,
			"{run}: {settled:?}"
		);
		let output: String = fs::read_to_string(artifacts_dir.join(path)).expect("read settled");
		assert!(output.contains(winner), "{run}: {path}: {output}");
	}
	assert_eq!(
		unit_for(merged, BROKEN_PATH).disposition,
		MergeDisposition::UnsupportedInput,
		"{run}"
	);
	let module: &MergePlanEntry = frozen.module();
	let module_unit: &MergeUnitOutcome = unit_for(merged, module.output_path());
	assert_eq!(module_unit.disposition, MergeDisposition::Safe, "{run}");
	assert_eq!(module_unit.output_paths.len(), 2, "{run}: {module_unit:?}");
	assert_eq!(
		unit_for(merged, LOCALISATION_PATH).disposition,
		MergeDisposition::Safe,
		"{run}"
	);
	assert_eq!(
		unit_for(merged, OVERLAY_PATH).disposition,
		MergeDisposition::Copy,
		"{run}"
	);
	assert_eq!(
		unit_for(merged, "gfx/interface/only_a.dds").disposition,
		MergeDisposition::Copy,
		"{run}"
	);
	// Several units append to the order-sensitive lists, in plan order.
	let deferred: Vec<&str> = report
		.conflict_resolutions
		.iter()
		.map(|resolution| resolution.path.as_str())
		.filter(|path| path.starts_with("history/countries/"))
		.collect();
	assert_eq!(
		deferred,
		[
			EARLY_CONFLICT_PATH,
			GENUINE_CONFLICT_PATH,
			LATE_CONFLICT_PATH
		],
		"{run}"
	);
	assert!(
		warning_position(report, EARLY_CONFLICT_PATH)
			< warning_position(report, GENUINE_CONFLICT_PATH)
			&& warning_position(report, GENUINE_CONFLICT_PATH)
				< warning_position(report, LATE_CONFLICT_PATH)
			&& warning_position(report, LATE_CONFLICT_PATH) < warning_position(report, BROKEN_PATH),
		"{run}: {:?}",
		report.warnings
	);
	// The address-patch backend records how the settled conflict was
	// resolved; the overlap test checks the order of several such records.
	if backend == MergeBackendId::AddressPatch {
		assert!(
			report
				.handler_resolutions
				.iter()
				.any(|record| record.path == DOWNSTREAM_RESOLVED_PATH),
			"{run}: {:?}",
			report.handler_resolutions
		);
	}
	// Provenance adds per-definition facts that also have to match.
	assert_eq!(!report.merge_trace.is_empty(), provenance, "{run}");
	assert_eq!(
		!report.definition_provenance.is_empty(),
		provenance,
		"{run}"
	);
}

#[test]
fn parallel_output_matches_one_worker() {
	let temp: TempDir = TempDir::new().unwrap();
	write_mixed_playset(temp.path());
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	// Safe files, five sibling conflicts and the module.
	assert_eq!(
		frozen.count(MergePlanStrategy::StructuralMerge),
		SAFE_FILE_COUNT + 6,
		"{:?}",
		frozen
			.plan
			.paths
			.iter()
			.map(|entry| (entry.output_path(), entry.strategy))
			.collect::<Vec<_>>()
	);
	assert_eq!(frozen.count(MergePlanStrategy::ManualConflict), 1);
	assert_eq!(
		frozen.entry(BROKEN_PATH).strategy,
		MergePlanStrategy::ManualConflict
	);
	assert_eq!(frozen.count(MergePlanStrategy::LocalisationMerge), 1);
	assert_eq!(
		frozen.entry(OVERLAY_PATH).strategy,
		MergePlanStrategy::LastWriterOverlay
	);
	assert!(frozen.count(MergePlanStrategy::CopyThrough) >= 2);
	assert_eq!(frozen.module().target.output_paths().len(), 2);
	let plan_order: Vec<String> = frozen.output_paths();
	assert!(
		plan_position(&plan_order, EARLY_CONFLICT_PATH)
			< plan_position(&plan_order, LATE_CONFLICT_PATH)
	);
	// Each backend once without and once with --force; provenance alternates.
	for (backend, force, provenance) in [
		(MergeBackendId::AddressPatch, false, false),
		(MergeBackendId::AddressPatch, true, true),
		(MergeBackendId::GumtreePcsNway, false, true),
		(MergeBackendId::GumtreePcsNway, true, false),
	] {
		let mut serial: Option<RunSnapshot> = None;
		for worker_count in [1, 2, 4, 8] {
			let run: String = format!(
				"{}-force-{force}-provenance-{provenance}-workers-{worker_count}",
				backend.as_str()
			);
			// With several workers the early conflict finishes after the late
			// one, so results arrive out of plan order.
			let hold: bool = worker_count > 1;
			let (wrapped, recorder) =
				recording_over(backend_for(backend), move |attempt, recorder, proceed| {
					if hold && attempt.target == EARLY_CONFLICT_PATH {
						recorder.exits.wait_for(&[LATE_CONFLICT_PATH]);
					}
					proceed()
				});
			let mut options: MergeMaterializeOptions = options_with(wrapped, worker_count);
			options.force = force;
			options.provenance = provenance;
			options.resolution_map = chosen_resolution();
			let (result, artifacts_dir) = frozen.materialize(&run, options);
			let merged: MaterializedMerge = result.unwrap_or_else(|error| panic!("{run}: {error}"));
			assert!(
				!recorder.exits.timed_out(),
				"{run}: the late conflict never finished"
			);
			if hold {
				let early: Call = recorder.call(EARLY_CONFLICT_PATH);
				let late: Call = recorder.call(LATE_CONFLICT_PATH);
				assert!(late.exited < early.exited, "{run}: {early:?} {late:?}");
				assert!(early.on_worker() && late.on_worker(), "{run}");
			}
			let snapshot: RunSnapshot = RunSnapshot::capture(&merged, &artifacts_dir);
			match &serial {
				None => {
					assert_mixed_run_is_not_vacuous(
						backend,
						&frozen,
						&merged,
						&artifacts_dir,
						force,
						provenance,
						&run,
					);
					serial = Some(snapshot);
				}
				Some(serial) => serial.assert_matches(&snapshot, &run),
			}
		}
	}
}

/// A conflict that a unit reports without any backend work, with one handler
/// record, so applying it appends to every order-sensitive report list.
fn probe_conflict(target: &str) -> BackendOutcome {
	Err(StructuralMergeFailure::Unresolved(
		StructuralConflictReport {
			reason: format!("probe conflict in {target}"),
			leaf_conflicts: Vec::new(),
			handler_resolutions: vec![HandlerResolutionRecord {
				path: target.to_string(),
				action: "probe".to_string(),
				source: None,
				rationale: None,
			}],
			explicitly_deferred: false,
		},
	))
}

#[test]
fn parallel_workers_overlap_and_never_exceed_the_limit() {
	const WORKERS: usize = 3;
	const UNITS: usize = 3 * WORKERS;
	let temp: TempDir = TempDir::new().unwrap();
	write_province_playset(temp.path(), UNITS);
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	assert_eq!(frozen.count(MergePlanStrategy::StructuralMerge), UNITS);
	let plan_order: Vec<String> = frozen.output_paths();
	assert_eq!(
		plan_order,
		(0..UNITS).map(province_path).collect::<Vec<_>>()
	);
	// Within each batch of three, a unit waits for the next one to finish, so
	// all three are in flight together and they finish in reverse plan order.
	let (backend, recorder) = recording(move |attempt, recorder, _| {
		let index: usize = province_index(attempt.target);
		if index % 3 < 2 {
			recorder.exits.wait_for(&[&province_path(index + 1)]);
		} else {
			// Two units of the batch are waiting on this one: a worker beyond
			// the limit would start another unit now.
			thread::sleep(EXTRA_WORKER_WINDOW);
		}
		if index % 3 == 1 {
			return probe_conflict(attempt.target);
		}
		let mut output: StructuralMergeOutput = structural_merge_output("probe = yes\n");
		output
			.stale_vanilla_targets
			.push(StaleVanillaTargetDescriptor {
				file_path: attempt.target.to_string(),
				..StaleVanillaTargetDescriptor::default()
			});
		Ok(output)
	});
	let (result, _) = frozen.materialize("workers-3", options_with(backend, WORKERS));
	let merged: MaterializedMerge = result.unwrap();
	assert!(!recorder.exits.timed_out(), "a batch never filled");
	assert_eq!(recorder.max_active(), WORKERS);
	let calls: Vec<Call> = recorder.calls();
	assert_eq!(calls.len(), UNITS);
	assert!(calls.iter().all(Call::on_worker), "{calls:?}");
	let threads: BTreeSet<&str> = calls.iter().map(|call| call.thread_name.as_str()).collect();
	assert_eq!(threads.len(), WORKERS, "{threads:?}");
	for batch in 0..UNITS / 3 {
		let [first, second, third]: [Call; 3] =
			[0, 1, 2].map(|offset| recorder.call(&province_path(3 * batch + offset)));
		assert!(
			third.exited < second.exited && second.exited < first.exited,
			"batch {batch} did not finish in reverse: {first:?} {second:?} {third:?}"
		);
	}
	// Every list applying appends to follows plan order, not completion order.
	let conflicts: Vec<String> = (0..UNITS)
		.filter(|index| index % 3 == 1)
		.map(province_path)
		.collect();
	let merges: Vec<String> = (0..UNITS)
		.filter(|index| index % 3 != 1)
		.map(province_path)
		.collect();
	let report: &MergeReport = &merged.report;
	let stale: Vec<String> = report
		.stale_vanilla_targets
		.iter()
		.map(|target| target.file_path.clone())
		.collect();
	assert_eq!(stale, merges);
	let deferred: Vec<String> = report
		.conflict_resolutions
		.iter()
		.map(|resolution| resolution.path.clone())
		.collect();
	assert_eq!(deferred, conflicts);
	let handled: Vec<String> = report
		.handler_resolutions
		.iter()
		.map(|record| record.path.clone())
		.collect();
	assert_eq!(handled, conflicts);
	let warned: Vec<usize> = conflicts
		.iter()
		.map(|path| warning_position(report, path))
		.collect();
	assert!(warned.is_sorted(), "{:?}", report.warnings);
	for (index, path) in plan_order.iter().enumerate() {
		let expected: MergeDisposition = if index % 3 == 1 {
			MergeDisposition::NeedsUserChoice
		} else {
			MergeDisposition::Safe
		};
		assert_eq!(unit_for(&merged, path).disposition, expected, "{path}");
	}
	assert_eq!(report.status, MergeReportStatus::PartialSuccess);
}

#[test]
fn a_definition_module_is_analyzed_once_on_one_worker() {
	const FILES: usize = 6;
	let temp: TempDir = TempDir::new().unwrap();
	write_database_playset(temp.path(), FILES);
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	let namespaces: Vec<String> = frozen
		.module()
		.target
		.output_paths()
		.into_iter()
		.map(str::to_string)
		.collect();
	assert_eq!(namespaces.len(), 2);
	// The module comes first, so a namespace dispatched as a job of its own
	// would reach a free worker while the first namespace is held.
	assert_eq!(frozen.output_paths()[0], frozen.module().output_path());
	let first_namespace: String = namespaces[0].clone();
	let (backend, recorder) = recording(move |attempt, recorder, proceed| {
		if attempt.module && attempt.target == first_namespace {
			let files: Vec<String> = (0..FILES).map(province_path).collect();
			let files: Vec<&str> = files.iter().map(String::as_str).collect();
			recorder.exits.wait_for(&files);
		}
		proceed()
	});
	let (result, artifacts_dir) = frozen.materialize("workers-4", options_with(backend, 4));
	let merged: MaterializedMerge = result.unwrap();
	assert!(!recorder.exits.timed_out(), "the file units never finished");
	let mut module_calls: Vec<Call> = recorder
		.calls()
		.into_iter()
		.filter(|call| call.module)
		.collect();
	module_calls.sort_by_key(|call| call.entered);
	let analyzed: Vec<&str> = module_calls
		.iter()
		.map(|call| call.target.as_str())
		.collect();
	assert_eq!(analyzed, namespaces, "each namespace once, in output order");
	let [first, second] = module_calls.as_slice() else {
		unreachable!("two namespace calls asserted above");
	};
	assert!(first.on_worker(), "{first:?}");
	assert_eq!(first.thread, second.thread, "{module_calls:?}");
	assert!(
		first.exited < second.entered,
		"namespaces overlapped: {module_calls:?}"
	);
	let module_unit: &MergeUnitOutcome = unit_for(&merged, frozen.module().output_path());
	assert_eq!(module_unit.disposition, MergeDisposition::Safe);
	assert_eq!(module_unit.output_paths, namespaces);
	for namespace in &namespaces {
		assert!(artifacts_dir.join(namespace).is_file(), "{namespace}");
	}
}

#[test]
fn a_module_failing_in_its_second_namespace_writes_neither_directory() {
	let temp: TempDir = TempDir::new().unwrap();
	write_database_playset(temp.path(), 6);
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	let module_path: String = frozen.module().output_path().to_string();
	let namespaces: Vec<String> = frozen
		.module()
		.target
		.output_paths()
		.into_iter()
		.map(str::to_string)
		.collect();
	let mut serial: Option<RunSnapshot> = None;
	for worker_count in [1, 4] {
		let failing: String = namespaces[1].clone();
		let (backend, recorder) = recording(move |attempt, _, proceed| {
			if attempt.module && attempt.target == failing {
				return Err(StructuralMergeFailure::Merge(MergeError::Validation {
					path: Some(attempt.target.to_string()),
					message: "controlled second-namespace failure".to_string(),
				}));
			}
			proceed()
		});
		let run: String = format!("workers-{worker_count}");
		let (result, artifacts_dir) = frozen.materialize(&run, options_with(backend, worker_count));
		let merged: MaterializedMerge = result.unwrap();
		// The first namespace merged and was staged before the second failed.
		let analyzed: Vec<String> = recorder
			.calls()
			.into_iter()
			.filter(|call| call.module)
			.map(|call| call.target)
			.collect();
		assert_eq!(analyzed.len(), 2, "{run}: {analyzed:?}");
		let module_unit: &MergeUnitOutcome = unit_for(&merged, &module_path);
		assert_eq!(
			module_unit.disposition,
			MergeDisposition::EngineFailure,
			"{run}"
		);
		assert!(module_unit.output_paths.is_empty(), "{run}");
		for namespace in &namespaces {
			assert!(
				!artifacts_dir.join(namespace).exists(),
				"{run}: {namespace}"
			);
		}
		let staging_left: Vec<String> = fs::read_dir(artifacts_dir.join(".foch"))
			.unwrap()
			.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
			.filter(|name| name.starts_with("module-stage-"))
			.collect();
		assert!(staging_left.is_empty(), "{run}: {staging_left:?}");
		for index in 0..6 {
			let path: String = province_path(index);
			assert_eq!(
				unit_for(&merged, &path).disposition,
				MergeDisposition::Safe,
				"{run}"
			);
			assert!(artifacts_dir.join(&path).is_file(), "{run}: {path}");
		}
		assert_eq!(merged.report.status, MergeReportStatus::PartialSuccess);
		assert_eq!(merged.report.engine_failure_count, 1);
		let snapshot: RunSnapshot = RunSnapshot::capture(&merged, &artifacts_dir);
		match &serial {
			None => serial = Some(snapshot),
			Some(serial) => serial.assert_matches(&snapshot, &run),
		}
	}
}

#[test]
fn cancelling_mid_run_returns_cancelled_and_leaves_no_worker_running() {
	const UNITS: usize = 12;
	let temp: TempDir = TempDir::new().unwrap();
	write_province_playset(temp.path(), UNITS);
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	assert_eq!(frozen.count(MergePlanStrategy::StructuralMerge), UNITS);
	assert_eq!(frozen.plan.paths.len(), UNITS);
	for worker_count in [1, 4] {
		let cancellation: CancellationToken = CancellationToken::new();
		let cancel: CancellationToken = cancellation.clone();
		let (backend, recorder) = recording(move |attempt, _, proceed| {
			if attempt.position == 3 {
				cancel.cancel();
			}
			proceed()
		});
		let mut options: MergeMaterializeOptions = options_with(backend, worker_count);
		options.cancellation = cancellation;
		let run: String = format!("workers-{worker_count}");
		let (result, _) = frozen.materialize(&run, options);
		let started_at_return: usize = recorder.started();
		let active_at_return: usize = recorder.active();
		assert!(
			matches!(result, Err(MergeError::Cancelled)),
			"{run}: {:?}",
			result.as_ref().err()
		);
		assert_eq!(active_at_return, 0, "{run}: an analysis outlived the call");
		if worker_count == 1 {
			// The serial loop checks before every unit and stops at the next.
			assert_eq!(started_at_return, 3, "{run}");
		} else {
			// Units already running when the token was cancelled finish; the
			// executor tests pin down that queued ones are skipped.
			assert!(
				(3..3 + worker_count).contains(&started_at_return),
				"{run}: {started_at_return}"
			);
		}
		// Nothing keeps running after the call returns.
		assert_eq!(recorder.started(), started_at_return, "{run}");
		assert_eq!(recorder.calls().len(), started_at_return, "{run}");
	}
}

/// Defers every conflict and records the thread each prompt ran on.
struct RecordingHandler {
	threads: Arc<Mutex<Vec<ThreadId>>>,
}

impl ConflictHandler for RecordingHandler {
	fn on_conflict(&mut self, _: &ConflictView) -> ConflictDecision {
		lock(&self.threads).push(thread::current().id());
		ConflictDecision::Defer { record: None }
	}
}

#[test]
fn an_interactive_handler_keeps_analysis_on_the_calling_thread() {
	let temp: TempDir = TempDir::new().unwrap();
	write_mixed_playset(temp.path());
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	let (backend, recorder) = recording(|_, _, proceed| proceed());
	let prompts: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
	let mut options: MergeMaterializeOptions = options_with(backend, 4);
	options.interactive_conflict_handler = Some(Box::new(RecordingHandler {
		threads: Arc::clone(&prompts),
	}));
	options.interactive_resolution_config_path = Some(temp.path().join("foch.toml"));
	let (result, _) = frozen.materialize("interactive", options);
	let merged: MaterializedMerge = result.unwrap();
	assert_eq!(
		unit_for(&merged, GENUINE_CONFLICT_PATH).disposition,
		MergeDisposition::NeedsUserChoice
	);
	let test_thread: ThreadId = thread::current().id();
	let prompted: Vec<ThreadId> = lock(&prompts).clone();
	assert!(!prompted.is_empty(), "no conflict reached the handler");
	assert!(prompted.iter().all(|thread| *thread == test_thread));
	let calls: Vec<Call> = recorder.calls();
	assert!(calls.len() > SAFE_FILE_COUNT, "{calls:?}");
	assert!(
		calls.iter().all(|call| call.thread == test_thread),
		"{calls:?}"
	);
}

#[test]
fn the_first_fatal_error_in_plan_order_matches_one_worker() {
	const OVERLAY: &str = "common/overlay.txt";
	let temp: TempDir = TempDir::new().unwrap();
	write_province_playset(temp.path(), 6);
	write_file(&temp.path().join("par-a"), OVERLAY, "from a\n");
	write_file(&temp.path().join("par-b"), OVERLAY, "from b\n");
	let frozen: FrozenPlayset = FrozenPlayset::freeze(temp.path());
	let plan_order: Vec<String> = frozen.output_paths();
	assert_eq!(plan_order[0], OVERLAY);
	assert_eq!(
		frozen.entry(OVERLAY).strategy,
		MergePlanStrategy::LastWriterOverlay
	);
	let early: String = province_path(0);
	let late: String = province_path(5);
	assert!(plan_position(&plan_order, &early) < plan_position(&plan_order, &late));

	// Two structural units fail when applied. On several workers the later
	// one finishes its analysis first, and the earlier one still decides the
	// error.
	let run_structural_failures = |worker_count: usize| -> String {
		let (early_target, late_target): (String, String) = (early.clone(), late.clone());
		let (backend, recorder) = recording(move |attempt, recorder, _| {
			if attempt.target == late_target {
				return missing_payload_output("late-payload", attempt.target);
			}
			if attempt.target == early_target {
				if worker_count > 1 {
					recorder.exits.wait_for(&[&late_target]);
				}
				return missing_payload_output("early-payload", attempt.target);
			}
			Ok(structural_merge_output("probe = yes\n"))
		});
		let run: String = format!("structural-workers-{worker_count}");
		let (result, _) = frozen.materialize(&run, options_with(backend, worker_count));
		assert!(
			!recorder.exits.timed_out(),
			"{run}: the later unit never ran"
		);
		match result {
			Ok(_) => panic!("{run}: the merge must fail"),
			Err(error) => error.to_string(),
		}
	};
	let serial: String = run_structural_failures(1);
	assert!(serial.contains("early-payload"), "{serial}");
	assert!(serial.contains(&early), "{serial}");
	assert_eq!(run_structural_failures(4), serial);

	// A copied unit ahead of every structural merge fails once its winning
	// source disappears after the plan was frozen. It is applied on the
	// calling thread, so this checks that the error surfaces the same way.
	let winner: PathBuf = temp.path().join("par-b").join(OVERLAY);
	fs::remove_file(&winner).unwrap();
	let run_overlay_failure = |worker_count: usize| -> String {
		let late_target: String = late.clone();
		let (backend, _) = recording(move |attempt, _, proceed| {
			if attempt.target == late_target {
				return missing_payload_output("late-payload", attempt.target);
			}
			proceed()
		});
		let run: String = format!("overlay-workers-{worker_count}");
		let (result, _) = frozen.materialize_frozen(&run, options_with(backend, worker_count));
		match result {
			Ok(_) => panic!("{run}: the merge must fail"),
			Err(error) => error.to_string(),
		}
	};
	let serial: String = run_overlay_failure(1);
	assert!(serial.starts_with("io error"), "{serial}");
	assert_eq!(run_overlay_failure(4), serial);
}
