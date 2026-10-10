//! Bounded combinatorial merge probe over installed Workshop mods.
//!
//! It composes singletons, then the most-overlapping pairs, then growing
//! precedence prefixes of an ordered mod universe, runs the exact Cargo-built
//! `foch merge` in analysis-only mode on each, and delta-debugs every new
//! failure signature down to the smallest sub-combination that still
//! reproduces it. Outcomes are append-only and keyed by executable, game and
//! ordered Workshop identities, so a later invocation resumes where an
//! interrupted or budget-limited one stopped. This is bug discovery, never an
//! acceptance result: it neither uses nor shrinks the fixed Workshop cohort.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use foch::game::eu4::base::snapshot;
use foch::playset::descriptor::load_descriptor;
use foch::project::{Project, ProjectConfig, ProjectMod};
use serde::{Deserialize, Serialize};

use super::{
	LibraryPair, ProbeResult, UnavailableInput, ensure_base, foch_command, installed_libraries,
	normalize_pairs, process, resolve_installed, write_json,
};
use crate::merge_quality::config::{
	DiscoveryOverrides, Eu4GameDiscovery, WorkshopItemVersion, discover_eu4_game,
};
use crate::merge_quality::lifecycle::executable_hash;

const OUTCOME_SCHEMA: &str = "foch-combination-probe-outcome-v1";
const MINIMIZED_SCHEMA: &str = "foch-combination-probe-minimized-v1";
const OUTCOMES_FILE: &str = "outcomes.jsonl";
const MINIMIZED_FILE: &str = "minimized.jsonl";
/// Signature kinds that always count as a failure to minimize.
const HARD_FAILURES: [&str; 5] = ["panic", "error", "timeout", "fatal", "engine_failure"];
/// Review dispositions recorded as signatures; only `--fail-on` ones minimize.
const RECORDED_DISPOSITIONS: [&str; 4] = [
	"engine_failure",
	"needs_user_choice",
	"unsupported_input",
	"deferred",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
	Singleton,
	Pair,
	Prefix,
	Minimize,
}

impl Phase {
	fn parse(text: &str) -> ProbeResult<Self> {
		Ok(match text {
			"singleton" => Self::Singleton,
			"pair" => Self::Pair,
			"prefix" => Self::Prefix,
			_ => return Err(format!("unknown combination phase `{text}`").into()),
		})
	}
}

/// One installed mod of the ordered universe. Its index is its precedence.
#[derive(Clone, Debug, Serialize)]
pub(super) struct UniverseMod {
	pub id: String,
	pub manifest: String,
	pub name: String,
	pub content_path: PathBuf,
	/// Universe indices of its declared dependencies.
	pub dependencies: Vec<usize>,
	pub unresolved_dependencies: Vec<String>,
	#[serde(skip)]
	pub source: Option<WorkshopItemVersion>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub(super) struct ModRef {
	pub id: String,
	pub manifest: String,
}

#[derive(Clone, Debug)]
pub(super) struct Budget {
	/// New merges this invocation may start, minimization included.
	pub max_runs: usize,
	/// Largest growing prefix, before dependency closure.
	pub max_size: usize,
	pub phases: Vec<Phase>,
	/// Review dispositions minimized in addition to the hard failures.
	pub fail_on: BTreeSet<String>,
	pub timeout: Duration,
}

pub(super) struct CombinationConfig {
	pub root: PathBuf,
	pub game: Eu4GameDiscovery,
	pub base_data: PathBuf,
	pub universe: Vec<UniverseMod>,
	pub unavailable: Vec<UnavailableInput>,
	pub budget: Budget,
}

/// What one merge process left behind.
pub(super) struct Observed {
	pub exit_code: Option<i32>,
	pub timed_out: bool,
	pub elapsed_ms: u128,
	pub stdout: String,
	pub stderr: String,
}

pub(super) trait CombinationRunner {
	fn prepare(&mut self, config: &CombinationConfig, run: &Path) -> ProbeResult<()>;
	fn merge(
		&mut self,
		config: &CombinationConfig,
		case: &Path,
		mods: &[&UniverseMod],
	) -> ProbeResult<Observed>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Counts {
	pub total: usize,
	pub safe: usize,
	pub copy: usize,
	pub needs_user_choice: usize,
	pub unsupported_input: usize,
	pub engine_failure: usize,
	pub deferred: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub(super) struct Signature {
	/// Stable identity across combinations: kind plus family/path or location.
	pub key: String,
	pub kind: String,
	pub family: Option<String>,
	pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct OutcomeRecord {
	pub schema: String,
	pub executable_blake3: String,
	pub game_version: String,
	pub mods: Vec<ModRef>,
	pub phase: Phase,
	/// The analysis printed a complete review.
	pub completed: bool,
	pub status: Option<String>,
	pub exit_code: Option<i32>,
	pub timed_out: bool,
	pub elapsed_ms: u128,
	pub counts: Option<Counts>,
	pub signatures: Vec<Signature>,
	pub logs: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct MinimizedRecord {
	pub schema: String,
	pub executable_blake3: String,
	pub game_version: String,
	pub signature: Signature,
	pub from: Vec<ModRef>,
	/// The 1-minimal culprits, in precedence order.
	pub culprits: Vec<ModRef>,
	/// The culprits with their declared dependencies, as merged.
	pub reproduced_with: Vec<ModRef>,
}

#[test]
#[ignore = "real installed Workshop merges; run cargo combination-probe"]
fn installed_combinations() {
	crate::require_acceptance("workshop-combination-probe");
	let root: PathBuf = std::env::var_os("FOCH_COMBO_PROBE_DIR")
		.map(PathBuf::from)
		.unwrap_or_else(|| {
			PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/combination-probe")
		});
	fs::create_dir_all(&root).expect("create probe directory");
	let root = root.canonicalize().unwrap();
	let (config, libraries) = live_config(root).unwrap_or_else(|error| panic!("{error}"));
	let run = execute(&config, &mut CliRunner).unwrap_or_else(|error| panic!("{error}"));
	let ids: Vec<&str> = config.universe.iter().map(|m| m.id.as_str()).collect();
	let (after, _) = resolve_installed(&ids, &libraries).unwrap();
	let changed = after
		.iter()
		.zip(&config.universe)
		.any(|(after, before)| after.identity.manifest_id.to_string() != before.manifest);
	assert!(
		after.len() == config.universe.len() && !changed,
		"Workshop input identity changed during the probe; records stay keyed by the old identity ({})",
		run.display()
	);
}

fn env_number(name: &str, default: usize) -> ProbeResult<usize> {
	match std::env::var(name) {
		Ok(text) => Ok(text
			.parse()
			.map_err(|_| format!("{name} must be a non-negative integer"))?),
		Err(_) => Ok(default),
	}
}

fn env_list(name: &str) -> Option<Vec<String>> {
	std::env::var(name).ok().map(|text| {
		text.split(',')
			.map(str::trim)
			.filter(|item| !item.is_empty())
			.map(str::to_owned)
			.collect()
	})
}

fn live_config(root: PathBuf) -> ProbeResult<(CombinationConfig, Vec<LibraryPair>)> {
	let game = discover_eu4_game(&DiscoveryOverrides::default())?;
	let libraries = normalize_pairs(installed_libraries(&game)?)?;
	let (ids, mut unavailable) = match env_list("FOCH_COMBO_PROBE_MODS") {
		Some(ids) => (ids, Vec::new()),
		None => current_playset_ids()?,
	};
	let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
	let (universe, missing) = resolve_universe(&ids, &libraries)?;
	unavailable.extend(missing);
	if universe.is_empty() {
		return Err("no installed Workshop mod is available to combine".into());
	}
	let phases = env_list("FOCH_COMBO_PROBE_PHASES")
		.unwrap_or_else(|| vec!["singleton".into(), "pair".into(), "prefix".into()])
		.iter()
		.map(|phase| Phase::parse(phase))
		.collect::<ProbeResult<_>>()?;
	let fail_on: BTreeSet<String> = env_list("FOCH_COMBO_PROBE_FAIL_ON")
		.unwrap_or_default()
		.into_iter()
		.collect();
	if let Some(unknown) = fail_on
		.iter()
		.find(|kind| !RECORDED_DISPOSITIONS.contains(&kind.as_str()))
	{
		return Err(
			format!("FOCH_COMBO_PROBE_FAIL_ON names unknown disposition `{unknown}`").into(),
		);
	}
	let budget = Budget {
		max_runs: env_number("FOCH_COMBO_PROBE_MAX_RUNS", 100)?,
		max_size: env_number("FOCH_COMBO_PROBE_MAX_SIZE", usize::MAX)?,
		phases,
		fail_on,
		timeout: Duration::from_secs(env_number("FOCH_COMBO_PROBE_TIMEOUT_SECS", 30 * 60)? as u64),
	};
	Ok((
		CombinationConfig {
			root,
			game,
			base_data: snapshot::data_root(),
			universe,
			unavailable,
			budget,
		},
		libraries,
	))
}

/// The enabled Workshop mods of the current launcher playset, in its order.
fn current_playset_ids() -> ProbeResult<(Vec<String>, Vec<UnavailableInput>)> {
	let input = foch::input::inspect_current_eu4_input();
	let playset = input
		.playset
		.ok_or("no current EU4 launcher playset; set FOCH_COMBO_PROBE_MODS")?;
	let mut mods = playset.mods;
	mods.sort_by_key(|m| m.position);
	let mut ids = Vec::new();
	let mut unavailable = Vec::new();
	for m in mods.into_iter().filter(|m| m.enabled) {
		match m
			.workshop_id
			.filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
		{
			Some(id) => ids.push(id),
			None => unavailable.push(UnavailableInput {
				id: m.id,
				reason: "not an installed Workshop item".into(),
			}),
		}
	}
	Ok((ids, unavailable))
}

/// The requested Workshop items as an ordered universe whose declared
/// dependencies are resolved, by descriptor name, to other universe members.
pub(super) fn resolve_universe(
	ids: &[&str],
	libraries: &[LibraryPair],
) -> ProbeResult<(Vec<UniverseMod>, Vec<UnavailableInput>)> {
	let (sources, missing) = resolve_installed(ids, libraries)?;
	let descriptors = sources
		.iter()
		.map(|source| load_descriptor(&source.content_path.join("descriptor.mod")))
		.collect::<Result<Vec<_>, _>>()?;
	let by_name: HashMap<&str, usize> = descriptors
		.iter()
		.enumerate()
		.map(|(index, descriptor)| (descriptor.name.as_str(), index))
		.collect();
	let universe = sources
		.iter()
		.zip(&descriptors)
		.map(|(source, descriptor)| {
			let (resolved, unresolved): (Vec<&String>, Vec<&String>) = descriptor
				.dependencies
				.iter()
				.partition(|name| by_name.contains_key(name.as_str()));
			UniverseMod {
				id: source.identity.workshop_id.to_string(),
				manifest: source.identity.manifest_id.to_string(),
				name: descriptor.name.clone(),
				content_path: source.content_path.clone(),
				dependencies: resolved.iter().map(|name| by_name[name.as_str()]).collect(),
				unresolved_dependencies: unresolved.into_iter().cloned().collect(),
				source: Some(source.clone()),
			}
		})
		.collect();
	Ok((universe, missing))
}

pub(super) struct CliRunner;

impl CombinationRunner for CliRunner {
	fn prepare(&mut self, config: &CombinationConfig, run: &Path) -> ProbeResult<()> {
		ensure_base(&config.game, &config.base_data, &config.root, run)
	}

	fn merge(
		&mut self,
		config: &CombinationConfig,
		case: &Path,
		mods: &[&UniverseMod],
	) -> ProbeResult<Observed> {
		let project = Project {
			project: Some(ProjectConfig {
				game: Some(foch::game::eu4::Eu4),
				game_path: Some(config.game.game_root.clone()),
				paradox_data_path: Some(case.join("launcher")),
				mods: mods
					.iter()
					.enumerate()
					.map(|(position, m)| ProjectMod {
						id: Some(m.id.clone()),
						steam_id: Some(m.id.clone()),
						path: Some(m.content_path.clone()),
						workshop_identity: m.source.as_ref().map(|s| s.identity.clone()),
						enabled: true,
						position: Some(position),
					})
					.collect(),
				..ProjectConfig::default()
			}),
			..Project::default()
		};
		fs::write(case.join("foch.toml"), toml::to_string_pretty(&project)?)?;
		let mut command = foch_command(&config.base_data, &config.root, case);
		command
			.arg("merge")
			.arg(case.join("foch.toml"))
			.arg("--out")
			.arg(case.join("merged"))
			.args(["--non-interactive", "--review-all"]);
		let result = process::run(&mut command, case, "merge", config.budget.timeout)?;
		let read = |stream: &str| -> ProbeResult<String> {
			Ok(
				String::from_utf8_lossy(&fs::read(case.join(format!("merge.{stream}.log")))?)
					.into_owned(),
			)
		};
		Ok(Observed {
			exit_code: result.code,
			timed_out: result.timed_out,
			elapsed_ms: result.elapsed_ms,
			stdout: read("stdout")?,
			stderr: read("stderr")?,
		})
	}
}

/// The parsed analysis-only review printed by `foch merge`.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Review {
	pub status: String,
	pub counts: Counts,
	pub fatal_errors: Vec<String>,
	/// `(disposition, path, family)` for every listed unit.
	pub units: Vec<(String, String, String)>,
}

pub(super) fn parse_review(stdout: &str) -> Option<Review> {
	let start = stdout.find("Foch Merge Review\n")?;
	let mut review = Review::default();
	let mut section = "";
	let mut unit: Option<(String, String)> = None;
	let mut status = None;
	for line in stdout[start..].lines().skip(1) {
		if let Some(value) = line.strip_prefix("status: ") {
			status = Some(value.to_string());
		} else if let Some(value) = line.strip_prefix("units: ") {
			review.counts.total = value.trim().parse().ok()?;
			section = "counts";
		} else if line == "fatal errors:" {
			section = "fatal";
		} else if line == "review units:" {
			section = "units";
		} else if section == "counts"
			&& let Some((name, value)) = line.trim().split_once(": ")
		{
			let value: usize = value.parse().ok()?;
			match name {
				"safe" => review.counts.safe = value,
				"copy" => review.counts.copy = value,
				"needs_user_choice" => review.counts.needs_user_choice = value,
				"unsupported_input" => review.counts.unsupported_input = value,
				"engine_failure" => review.counts.engine_failure = value,
				"deferred" => review.counts.deferred = value,
				_ => return None,
			}
		} else if section == "fatal"
			&& let Some(error) = line.strip_prefix("  - ")
		{
			review.fatal_errors.push(error.to_string());
		} else if section == "units" {
			if let Some(rest) = line.strip_prefix("- [") {
				let (disposition, path) = rest.split_once("] ")?;
				unit = Some((disposition.to_string(), path.to_string()));
			} else if let Some(family) = line.strip_prefix("  family: ")
				&& let Some((disposition, path)) = unit.take()
			{
				review.units.push((disposition, path, family.to_string()));
			}
		}
	}
	review.status = status?;
	Some(review)
}

/// Removes this case's own directory from a message, so the same defect
/// has the same signature in every combination.
fn normalize(text: &str, case: &Path) -> String {
	let mut text = text.trim().to_string();
	let display = case.display().to_string();
	for form in [display.clone(), display.replace('\\', "/")] {
		if !form.is_empty() {
			text = text.replace(&form, "<case>");
		}
	}
	text
}

/// The outcome of one merge, with every recorded failure signature.
pub(super) fn classify(
	observed: &Observed,
	case: &Path,
) -> (bool, Option<String>, Option<Counts>, Vec<Signature>) {
	let mut signatures = BTreeSet::new();
	if observed.timed_out {
		signatures.insert(Signature {
			key: "timeout".into(),
			kind: "timeout".into(),
			family: None,
			detail: format!("no result after {} ms", observed.elapsed_ms),
		});
		return (false, None, None, signatures.into_iter().collect());
	}
	let review =
		parse_review(&observed.stdout).filter(|_| matches!(observed.exit_code, Some(0..=2)));
	let Some(review) = review else {
		signatures.insert(crash_signature(observed, case));
		return (false, None, None, signatures.into_iter().collect());
	};
	for error in &review.fatal_errors {
		let detail = normalize(error, case);
		signatures.insert(Signature {
			key: format!("fatal:{detail}"),
			kind: "fatal".into(),
			family: None,
			detail,
		});
	}
	for (disposition, path, family) in &review.units {
		if RECORDED_DISPOSITIONS.contains(&disposition.as_str()) {
			signatures.insert(Signature {
				key: format!("{disposition}:{family}:{path}"),
				kind: disposition.clone(),
				family: Some(family.clone()),
				detail: path.clone(),
			});
		}
	}
	(
		true,
		Some(review.status),
		Some(review.counts),
		signatures.into_iter().collect(),
	)
}

fn crash_signature(observed: &Observed, case: &Path) -> Signature {
	let lines: Vec<&str> = observed.stderr.lines().collect();
	// Rust prints `thread '<name>' panicked at <file:line:col>:` then the message.
	if let Some(index) = lines.iter().position(|line| line.contains(" panicked at ")) {
		let location = lines[index]
			.split(" panicked at ")
			.nth(1)
			.unwrap_or_default()
			.trim_end_matches(':');
		let message = lines
			.get(index + 1)
			.map(|line| normalize(line, case))
			.unwrap_or_default();
		return Signature {
			key: format!("panic:{location}"),
			kind: "panic".into(),
			family: None,
			detail: message,
		};
	}
	let last = lines
		.iter()
		.rev()
		.find(|line| !line.trim().is_empty())
		.map(|line| normalize(line, case))
		.unwrap_or_default();
	Signature {
		key: format!("error:exit {:?}: {last}", observed.exit_code),
		kind: "error".into(),
		family: None,
		detail: last,
	}
}

/// Relative file paths of a mod, lowercased with `/` separators.
fn mod_files(root: &Path) -> BTreeSet<String> {
	walkdir::WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter_map(|entry| {
			let relative = entry.path().strip_prefix(root).ok()?;
			let text = relative.to_string_lossy().replace('\\', "/").to_lowercase();
			text.contains('/').then_some(text)
		})
		.collect()
}

/// Pairs that share at least one top-level content directory, most
/// overlapping first. A pair touching disjoint top-level trees is only the
/// union of its singletons; the rest stay in precedence order on ties.
pub(super) fn ranked_pairs(universe: &[UniverseMod]) -> (Vec<(usize, usize)>, usize) {
	let files: Vec<BTreeSet<String>> = universe
		.iter()
		.map(|m| mod_files(&m.content_path))
		.collect();
	let parents = |set: &BTreeSet<String>, depth: usize| -> BTreeSet<String> {
		set.iter()
			.map(|path| {
				let parts: Vec<&str> = path.split('/').collect();
				parts[..parts.len().saturating_sub(1).min(depth)].join("/")
			})
			.collect()
	};
	let tops: Vec<BTreeSet<String>> = files.iter().map(|set| parents(set, 1)).collect();
	let dirs: Vec<BTreeSet<String>> = files.iter().map(|set| parents(set, usize::MAX)).collect();
	let mut ranked = Vec::new();
	let mut skipped = 0;
	for i in 0..universe.len() {
		for j in i + 1..universe.len() {
			if tops[i].is_disjoint(&tops[j]) {
				skipped += 1;
				continue;
			}
			let shared_files = files[i].intersection(&files[j]).count();
			let shared_dirs = dirs[i].intersection(&dirs[j]).count();
			ranked.push((shared_files, shared_dirs, i, j));
		}
	}
	ranked.sort_by(|a, b| (b.0, b.1, a.2, a.3).cmp(&(a.0, a.1, b.2, b.3)));
	(
		ranked.into_iter().map(|(_, _, i, j)| (i, j)).collect(),
		skipped,
	)
}

/// The set with every declared dependency inside the universe, in precedence order.
fn closure(universe: &[UniverseMod], set: &[usize]) -> Vec<usize> {
	let mut closed: BTreeSet<usize> = BTreeSet::new();
	let mut stack: Vec<usize> = set.to_vec();
	while let Some(index) = stack.pop() {
		if closed.insert(index) {
			stack.extend(&universe[index].dependencies);
		}
	}
	closed.into_iter().collect()
}

fn read_records<T: for<'de> Deserialize<'de>>(path: &Path) -> ProbeResult<Vec<T>> {
	if !path.exists() {
		return Ok(Vec::new());
	}
	let text = fs::read_to_string(path)?;
	let mut records = Vec::new();
	for (number, line) in text.lines().enumerate() {
		if line.trim().is_empty() {
			continue;
		}
		match serde_json::from_str(line) {
			Ok(record) => records.push(record),
			// An interrupted append leaves at most one torn final line.
			Err(_) if number + 1 == text.lines().count() && !text.ends_with('\n') => {}
			Err(error) => return Err(format!("{}:{}: {error}", path.display(), number + 1).into()),
		}
	}
	Ok(records)
}

fn append_record(path: &Path, record: &impl Serialize) -> ProbeResult<()> {
	let mut file = fs::OpenOptions::new()
		.create(true)
		.append(true)
		.open(path)?;
	let existing = file.metadata()?.len();
	let mut line = serde_json::to_vec(record)?;
	line.push(b'\n');
	if existing > 0 {
		// Never extend a torn final line from an interrupted append.
		let text = fs::read(path)?;
		if text.last() != Some(&b'\n') {
			line.insert(0, b'\n');
		}
	}
	file.write_all(&line)?;
	file.sync_data()?;
	Ok(())
}

struct Probe<'a, R: CombinationRunner> {
	config: &'a CombinationConfig,
	runner: &'a mut R,
	run: PathBuf,
	executable: String,
	outcomes: HashMap<Vec<ModRef>, OutcomeRecord>,
	minimized: Vec<MinimizedRecord>,
	runs_left: usize,
	started: usize,
	pairs_skipped: usize,
}

/// A budget-exhausted evaluation; the probe stops and a later run resumes.
struct OutOfBudget;

impl<R: CombinationRunner> Probe<'_, R> {
	fn refs(&self, set: &[usize]) -> Vec<ModRef> {
		set.iter()
			.map(|&index| ModRef {
				id: self.config.universe[index].id.clone(),
				manifest: self.config.universe[index].manifest.clone(),
			})
			.collect()
	}

	fn is_failure(&self, signature: &Signature) -> bool {
		HARD_FAILURES.contains(&signature.kind.as_str())
			|| self.config.budget.fail_on.contains(&signature.kind)
	}

	/// The outcome for a closed set, recorded or newly merged.
	fn evaluate(
		&mut self,
		set: &[usize],
		phase: Phase,
	) -> ProbeResult<Result<OutcomeRecord, OutOfBudget>> {
		let refs = self.refs(set);
		if let Some(record) = self.outcomes.get(&refs) {
			return Ok(Ok(record.clone()));
		}
		if self.runs_left == 0 {
			return Ok(Err(OutOfBudget));
		}
		self.runs_left -= 1;
		self.started += 1;
		let case = self.run.join(format!("{:04}", self.started));
		fs::create_dir_all(&case)?;
		let mods: Vec<&UniverseMod> = set.iter().map(|&i| &self.config.universe[i]).collect();
		eprintln!(
			"[combination-probe] {:?} #{} with {} mods: {}",
			phase,
			self.started,
			mods.len(),
			mods.iter()
				.map(|m| m.id.as_str())
				.collect::<Vec<_>>()
				.join(",")
		);
		let observed = self.runner.merge(self.config, &case, &mods)?;
		let (completed, status, counts, signatures) = classify(&observed, &case);
		let record = OutcomeRecord {
			schema: OUTCOME_SCHEMA.into(),
			executable_blake3: self.executable.clone(),
			game_version: self.config.game.game_version.clone(),
			mods: refs.clone(),
			phase,
			completed,
			status,
			exit_code: observed.exit_code,
			timed_out: observed.timed_out,
			elapsed_ms: observed.elapsed_ms,
			counts,
			signatures,
			logs: case,
		};
		append_record(&self.config.root.join(OUTCOMES_FILE), &record)?;
		self.outcomes.insert(refs, record.clone());
		self.write_summary()?;
		Ok(Ok(record))
	}

	fn reproduces(
		&mut self,
		subset: &[usize],
		key: &str,
	) -> ProbeResult<Result<bool, OutOfBudget>> {
		let closed = closure(&self.config.universe, subset);
		Ok(self
			.evaluate(&closed, Phase::Minimize)?
			.map(|record| record.signatures.iter().any(|s| s.key == key)))
	}

	/// Zeller's ddmin over the culprit candidates, in precedence order.
	fn ddmin(
		&mut self,
		mut current: Vec<usize>,
		key: &str,
	) -> ProbeResult<Result<Vec<usize>, OutOfBudget>> {
		let mut granularity = 2;
		while current.len() >= 2 {
			let size = current.len().div_ceil(granularity);
			let chunks: Vec<Vec<usize>> = current.chunks(size).map(<[usize]>::to_vec).collect();
			let mut reduced = false;
			for chunk in &chunks {
				match self.reproduces(chunk, key)? {
					Err(out) => return Ok(Err(out)),
					Ok(true) => {
						current = chunk.clone();
						granularity = 2;
						reduced = true;
						break;
					}
					Ok(false) => {}
				}
			}
			if !reduced && chunks.len() > 2 {
				for skip in 0..chunks.len() {
					let complement: Vec<usize> = chunks
						.iter()
						.enumerate()
						.filter(|(index, _)| *index != skip)
						.flat_map(|(_, chunk)| chunk.iter().copied())
						.collect();
					match self.reproduces(&complement, key)? {
						Err(out) => return Ok(Err(out)),
						Ok(true) => {
							current = complement;
							granularity = (granularity - 1).max(2);
							reduced = true;
							break;
						}
						Ok(false) => {}
					}
				}
			}
			if !reduced {
				if granularity >= current.len() {
					break;
				}
				granularity = (granularity * 2).min(current.len());
			}
		}
		Ok(Ok(current))
	}

	/// Minimizes each failure of a combination not already explained by a
	/// known culprit set inside it.
	fn attribute(
		&mut self,
		set: &[usize],
		record: &OutcomeRecord,
	) -> ProbeResult<Result<(), OutOfBudget>> {
		let refs = self.refs(set);
		let failures: Vec<Signature> = record
			.signatures
			.iter()
			.filter(|s| self.is_failure(s))
			.cloned()
			.collect();
		for signature in &failures {
			let explained = self.minimized.iter().any(|known| {
				known.signature.key == signature.key
					&& known.culprits.iter().all(|culprit| refs.contains(culprit))
			});
			if explained {
				continue;
			}
			let culprits = match self.ddmin(set.to_vec(), &signature.key)? {
				Err(out) => return Ok(Err(out)),
				Ok(culprits) => culprits,
			};
			let minimized = MinimizedRecord {
				schema: MINIMIZED_SCHEMA.into(),
				executable_blake3: self.executable.clone(),
				game_version: self.config.game.game_version.clone(),
				signature: signature.clone(),
				from: refs.clone(),
				culprits: self.refs(&culprits),
				reproduced_with: self.refs(&closure(&self.config.universe, &culprits)),
			};
			append_record(&self.config.root.join(MINIMIZED_FILE), &minimized)?;
			self.minimized.push(minimized);
			self.write_summary()?;
		}
		Ok(Ok(()))
	}

	fn plan(&mut self) -> Vec<(Phase, Vec<usize>)> {
		let config = self.config;
		let universe = &config.universe;
		let mut plan: Vec<(Phase, Vec<usize>)> = Vec::new();
		let mut seen = BTreeSet::new();
		let mut push = |phase: Phase, set: Vec<usize>| {
			let closed = closure(universe, &set);
			if seen.insert(closed.clone()) {
				plan.push((phase, closed));
			}
		};
		for phase in &config.budget.phases {
			match phase {
				Phase::Singleton => (0..universe.len()).for_each(|i| push(*phase, vec![i])),
				Phase::Pair => {
					let (pairs, skipped) = ranked_pairs(universe);
					self.pairs_skipped = skipped;
					pairs
						.into_iter()
						.for_each(|(i, j)| push(*phase, vec![i, j]));
				}
				Phase::Prefix => {
					let largest = universe.len().min(config.budget.max_size);
					(3..=largest).for_each(|size| push(*phase, (0..size).collect()));
				}
				Phase::Minimize => {}
			}
		}
		plan
	}

	fn write_summary(&self) -> ProbeResult<()> {
		let summary = summarize(
			self.config,
			&self.outcomes,
			&self.minimized,
			self.pairs_skipped,
		);
		write_json(&self.config.root.join("summary.json"), &summary)?;
		let markdown = render_summary(&summary);
		let mut temporary = tempfile::NamedTempFile::new_in(&self.config.root)?;
		temporary.write_all(markdown.as_bytes())?;
		temporary.persist(self.config.root.join("summary.md"))?;
		Ok(())
	}
}

pub(super) fn execute(
	config: &CombinationConfig,
	runner: &mut impl CombinationRunner,
) -> ProbeResult<PathBuf> {
	fs::create_dir_all(config.root.join("runs"))?;
	let lock = fs::OpenOptions::new()
		.create(true)
		.truncate(false)
		.read(true)
		.write(true)
		.open(config.root.join("probe.lock"))?;
	lock.try_lock().map_err(|error| {
		format!("probe directory is already in use or cannot be locked: {error}")
	})?;
	let run: PathBuf = tempfile::Builder::new()
		.prefix("run-")
		.tempdir_in(config.root.join("runs"))?
		.keep();
	eprintln!("[combination-probe] artifacts: {}", run.display());
	let executable = executable_hash(Path::new(env!("CARGO_BIN_EXE_foch")))?;
	write_json(
		&run.join("universe.json"),
		&serde_json::json!({
			"executable_blake3": executable,
			"game_version": config.game.game_version,
			"mods": config.universe,
			"unavailable": config.unavailable,
			"max_runs": config.budget.max_runs,
			"max_size": config.budget.max_size,
			"phases": config.budget.phases,
			"fail_on": config.budget.fail_on,
			"timeout_secs": config.budget.timeout.as_secs(),
		}),
	)?;
	runner.prepare(config, &run)?;
	let game_version = &config.game.game_version;
	let mut outcomes = HashMap::new();
	for record in read_records::<OutcomeRecord>(&config.root.join(OUTCOMES_FILE))? {
		if record.executable_blake3 == executable && &record.game_version == game_version {
			outcomes.insert(record.mods.clone(), record);
		}
	}
	let minimized: Vec<MinimizedRecord> = read_records(&config.root.join(MINIMIZED_FILE))?
		.into_iter()
		.filter(|record: &MinimizedRecord| {
			record.executable_blake3 == executable && &record.game_version == game_version
		})
		.collect();
	let mut probe = Probe {
		config,
		runner,
		run: run.clone(),
		executable,
		outcomes,
		minimized,
		runs_left: config.budget.max_runs,
		started: 0,
		pairs_skipped: 0,
	};
	let plan = probe.plan();
	let mut exhausted = false;
	for (phase, set) in plan {
		let record = match probe.evaluate(&set, phase)? {
			Ok(record) => record,
			Err(OutOfBudget) => {
				exhausted = true;
				break;
			}
		};
		if probe.attribute(&set, &record)?.is_err() {
			exhausted = true;
			break;
		}
	}
	probe.write_summary()?;
	eprintln!(
		"[combination-probe] {} new merges; {}; summary: {}",
		probe.started,
		if exhausted {
			"budget exhausted, rerun to resume"
		} else {
			"plan complete"
		},
		config.root.join("summary.md").display()
	);
	if executable_hash(Path::new(env!("CARGO_BIN_EXE_foch")))? != probe.executable {
		return Err("CLI artifact changed during the probe".into());
	}
	Ok(run)
}

#[derive(Debug, Serialize)]
pub(super) struct Bucket {
	pub signature: Signature,
	/// `mod`, `pair` or `group` by culprit count.
	pub attribution: String,
	pub culprits: Vec<String>,
	pub reproduced_with: Vec<ModRef>,
	/// Evaluated combinations holding the signature and every culprit.
	pub occurrences: usize,
	/// True when every evaluated singleton reproduces it, so it is not
	/// specific to the attributed mod.
	pub every_singleton: bool,
	pub example_logs: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(super) struct Summary {
	pub semantic_correctness: &'static str,
	pub universe: Vec<String>,
	pub evaluated: usize,
	pub completed: usize,
	pub failing_combinations: usize,
	pub pairs_skipped_as_disjoint: usize,
	pub buckets: Vec<Bucket>,
	/// Failure signatures no completed minimization explains yet.
	pub unattributed: Vec<Signature>,
	pub buckets_by_family: BTreeMap<String, usize>,
	pub singletons: Vec<serde_json::Value>,
}

fn summarize(
	config: &CombinationConfig,
	outcomes: &HashMap<Vec<ModRef>, OutcomeRecord>,
	minimized: &[MinimizedRecord],
	pairs_skipped: usize,
) -> Summary {
	let is_failure = |s: &Signature| {
		HARD_FAILURES.contains(&s.kind.as_str()) || config.budget.fail_on.contains(&s.kind)
	};
	let name = |r: &ModRef| {
		config
			.universe
			.iter()
			.find(|m| m.id == r.id)
			.map_or(r.id.clone(), |m| format!("{} ({})", m.name, m.id))
	};
	let singleton_outcomes: Vec<&OutcomeRecord> =
		outcomes.values().filter(|r| r.mods.len() == 1).collect();
	let mut seen = BTreeSet::new();
	let mut buckets = Vec::new();
	for known in minimized {
		if !seen.insert((known.signature.key.clone(), known.culprits.clone())) {
			continue;
		}
		let holds = |r: &OutcomeRecord| {
			r.signatures.iter().any(|s| s.key == known.signature.key)
				&& known.culprits.iter().all(|c| r.mods.contains(c))
		};
		buckets.push(Bucket {
			signature: known.signature.clone(),
			attribution: match known.culprits.len() {
				1 => "mod",
				2 => "pair",
				_ => "group",
			}
			.into(),
			culprits: known.culprits.iter().map(name).collect(),
			reproduced_with: known.reproduced_with.clone(),
			occurrences: outcomes.values().filter(|r| holds(r)).count(),
			every_singleton: singleton_outcomes.len() > 1
				&& singleton_outcomes
					.iter()
					.all(|r| r.signatures.iter().any(|s| s.key == known.signature.key)),
			example_logs: outcomes.get(&known.reproduced_with).map(|r| r.logs.clone()),
		});
	}
	buckets
		.sort_by(|a, b| (b.occurrences, &a.signature.key).cmp(&(a.occurrences, &b.signature.key)));
	let mut unattributed = BTreeSet::new();
	for record in outcomes.values() {
		for signature in record.signatures.iter().filter(|s| is_failure(s)) {
			let explained = minimized.iter().any(|known| {
				known.signature.key == signature.key
					&& known.culprits.iter().all(|c| record.mods.contains(c))
			});
			if !explained {
				unattributed.insert(signature.clone());
			}
		}
	}
	let mut buckets_by_family = BTreeMap::new();
	for bucket in &buckets {
		*buckets_by_family
			.entry(
				bucket
					.signature
					.family
					.clone()
					.unwrap_or_else(|| format!("<{}>", bucket.signature.kind)),
			)
			.or_insert(0) += 1;
	}
	let singletons = config
		.universe
		.iter()
		.filter_map(|m| {
			let key = vec![ModRef {
				id: m.id.clone(),
				manifest: m.manifest.clone(),
			}];
			outcomes.get(&key).map(|r| {
				serde_json::json!({
					"mod": format!("{} ({})", m.name, m.id),
					"status": r.status.clone().unwrap_or_else(|| "did_not_complete".into()),
					"counts": r.counts,
					"elapsed_ms": r.elapsed_ms,
				})
			})
		})
		.collect();
	Summary {
		semantic_correctness: "not_scored; a discovery record, not an acceptance result",
		universe: config
			.universe
			.iter()
			.map(|m| format!("{} ({})", m.name, m.id))
			.collect(),
		evaluated: outcomes.len(),
		completed: outcomes.values().filter(|r| r.completed).count(),
		failing_combinations: outcomes
			.values()
			.filter(|r| r.signatures.iter().any(&is_failure))
			.count(),
		pairs_skipped_as_disjoint: pairs_skipped,
		buckets,
		unattributed: unattributed.into_iter().collect(),
		buckets_by_family,
		singletons,
	}
}

fn render_summary(summary: &Summary) -> String {
	let mut text = format!(
		"# Combination probe\n\nDiscovery record for the current executable; not an acceptance result.\n\n- universe: {} mods\n- evaluated combinations: {} ({} completed analysis)\n- failing combinations: {}\n- pairs skipped as disjoint: {}\n- attributed buckets: {}\n- unattributed failure signatures: {}\n\n",
		summary.universe.len(),
		summary.evaluated,
		summary.completed,
		summary.failing_combinations,
		summary.pairs_skipped_as_disjoint,
		summary.buckets.len(),
		summary.unattributed.len(),
	);
	if !summary.buckets.is_empty() {
		text.push_str("## Minimized failures\n\n| kind | family | signature | attribution | culprits | occurrences |\n|---|---|---|---|---|---|\n");
		for bucket in &summary.buckets {
			text.push_str(&format!(
				"| {} | {} | `{}` | {}{} | {} | {} |\n",
				bucket.signature.kind,
				bucket.signature.family.as_deref().unwrap_or("-"),
				bucket.signature.key.replace('|', "\\|"),
				bucket.attribution,
				if bucket.every_singleton {
					" (every singleton)"
				} else {
					""
				},
				bucket.culprits.join(", ").replace('|', "\\|"),
				bucket.occurrences,
			));
		}
		text.push('\n');
	}
	if !summary.unattributed.is_empty() {
		text.push_str("## Not yet minimized (rerun to resume)\n\n");
		for signature in &summary.unattributed {
			text.push_str(&format!("- `{}`\n", signature.key));
		}
		text.push('\n');
	}
	text
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::write_file;

	fn universe_mod(root: &Path, id: &str, files: &[&str]) -> UniverseMod {
		let content = root.join(id);
		for file in files {
			write_file(&content, file, "x = 1\n");
		}
		UniverseMod {
			id: id.into(),
			manifest: format!("100{id}"),
			name: format!("fixture {id}"),
			content_path: content,
			dependencies: Vec::new(),
			unresolved_dependencies: Vec::new(),
			source: None,
		}
	}

	fn config(root: &Path, universe: Vec<UniverseMod>, budget: Budget) -> CombinationConfig {
		CombinationConfig {
			root: root.to_path_buf(),
			game: Eu4GameDiscovery {
				game_root: root.join("game"),
				game_version: "v1.37.5.0".into(),
				steam_build_id: None,
				steam_root: None,
			},
			base_data: root.join("data"),
			universe,
			unavailable: Vec::new(),
			budget,
		}
	}

	fn budget(max_runs: usize) -> Budget {
		Budget {
			max_runs,
			max_size: usize::MAX,
			phases: vec![Phase::Singleton, Phase::Pair, Phase::Prefix],
			fail_on: BTreeSet::new(),
			timeout: Duration::from_secs(60),
		}
	}

	/// Fails with an engine failure whenever both `b` and `d` are merged,
	/// and panics whenever `c` is.
	struct Fake {
		merges: Vec<Vec<String>>,
	}

	impl CombinationRunner for Fake {
		fn prepare(&mut self, _: &CombinationConfig, _: &Path) -> ProbeResult<()> {
			Ok(())
		}

		fn merge(
			&mut self,
			_: &CombinationConfig,
			case: &Path,
			mods: &[&UniverseMod],
		) -> ProbeResult<Observed> {
			let ids: Vec<String> = mods.iter().map(|m| m.id.clone()).collect();
			self.merges.push(ids.clone());
			let has = |id: &str| ids.iter().any(|m| m == id);
			if has("c") {
				return Ok(Observed {
					exit_code: Some(101),
					timed_out: false,
					elapsed_ms: 1,
					stdout: String::new(),
					stderr: format!(
						"thread 'main' panicked at src/merge.rs:10:5:\nbad unit in {}\n",
						case.display()
					),
				});
			}
			let broken = has("b") && has("d");
			let mut stdout = format!(
				"Foch Merge Review\nstatus: {}\nunits: 2\n  safe: 1\n  copy: 0\n  needs_user_choice: 0\n  unsupported_input: 0\n  engine_failure: {}\n  deferred: 0\nreview units:\n- [safe] common/ideas/a.txt\n  id: u1\n  family: common/ideas\n  kind: file\n  strategy: s\n  summary: s\n",
				if broken { "blocked" } else { "ready_to_commit" },
				usize::from(broken),
			);
			if broken {
				stdout.push_str("- [engine_failure] common/scripted_effects/x.txt\n  id: u2\n  family: common/scripted_effects\n  kind: definition_module\n  strategy: s\n  summary: s\n");
			}
			Ok(Observed {
				exit_code: Some(if broken { 2 } else { 0 }),
				timed_out: false,
				elapsed_ms: 1,
				stdout,
				stderr: String::new(),
			})
		}
	}

	fn summary(root: &Path) -> serde_json::Value {
		serde_json::from_slice(&fs::read(root.join("summary.json")).unwrap()).unwrap()
	}

	#[test]
	fn failures_are_minimized_to_a_mod_and_a_pair_and_resume_from_records() {
		let temp = tempfile::tempdir().unwrap();
		let root = temp.path().canonicalize().unwrap();
		let universe: Vec<UniverseMod> = ["a", "b", "c", "d", "e"]
			.into_iter()
			.map(|id| universe_mod(&root.join("mods"), id, &["common/ideas/x.txt"]))
			.collect();
		let config = config(&root, universe, budget(1000));
		let mut fake = Fake { merges: Vec::new() };
		execute(&config, &mut fake).unwrap();
		let summary = summary(&root);
		let buckets = summary["buckets"].as_array().unwrap();
		let panic = buckets
			.iter()
			.find(|b| b["signature"]["kind"] == "panic")
			.unwrap();
		assert_eq!(panic["attribution"], "mod");
		assert_eq!(panic["culprits"][0], "fixture c (c)");
		assert_eq!(panic["signature"]["key"], "panic:src/merge.rs:10:5");
		assert!(
			panic["signature"]["detail"]
				.as_str()
				.unwrap()
				.contains("<case>")
		);
		let engine = buckets
			.iter()
			.find(|b| b["signature"]["kind"] == "engine_failure")
			.unwrap();
		assert_eq!(engine["attribution"], "pair");
		assert_eq!(engine["culprits"][0], "fixture b (b)");
		assert_eq!(engine["culprits"][1], "fixture d (d)");
		assert_eq!(engine["signature"]["family"], "common/scripted_effects");
		assert_eq!(buckets.len(), 2, "same causes must not be re-reported");
		assert_eq!(summary["unattributed"].as_array().unwrap().len(), 0);
		// Every combination was planned before; resuming merges nothing again.
		let first = fake.merges.len();
		let mut resumed = Fake { merges: Vec::new() };
		execute(&config, &mut resumed).unwrap();
		assert!(resumed.merges.is_empty(), "{:?}", resumed.merges);
		assert!(first > 0);
		assert_eq!(
			fs::read_to_string(root.join(OUTCOMES_FILE))
				.unwrap()
				.lines()
				.count(),
			first
		);
	}

	#[test]
	fn a_budget_limited_run_stops_and_the_next_one_continues() {
		let temp = tempfile::tempdir().unwrap();
		let root = temp.path().canonicalize().unwrap();
		let universe: Vec<UniverseMod> = ["a", "b", "d"]
			.into_iter()
			.map(|id| universe_mod(&root.join("mods"), id, &["common/ideas/x.txt"]))
			.collect();
		let limited = config(&root, universe.clone(), budget(2));
		let mut fake = Fake { merges: Vec::new() };
		execute(&limited, &mut fake).unwrap();
		assert_eq!(fake.merges, [vec!["a"], vec!["b"]]);
		let full = config(&root, universe, budget(1000));
		let mut resumed = Fake { merges: Vec::new() };
		execute(&full, &mut resumed).unwrap();
		assert_eq!(resumed.merges[0], vec!["d"]);
		assert!(!resumed.merges.contains(&vec!["a".to_string()]));
		let buckets = summary(&root)["buckets"].as_array().unwrap().clone();
		assert_eq!(buckets.len(), 1);
		assert_eq!(buckets[0]["attribution"], "pair");
	}

	#[test]
	fn combinations_keep_precedence_and_close_over_declared_dependencies() {
		let temp = tempfile::tempdir().unwrap();
		let root = temp.path();
		let mut universe = vec![
			universe_mod(root, "base", &["common/ideas/a.txt"]),
			universe_mod(root, "gfx", &["gfx/a.dds"]),
			universe_mod(root, "sub", &["common/ideas/a.txt", "common/ideas/b.txt"]),
		];
		universe[2].dependencies = vec![0];
		let (pairs, skipped) = ranked_pairs(&universe);
		assert_eq!(pairs, [(0, 2)]);
		assert_eq!(skipped, 2);
		assert_eq!(closure(&universe, &[2]), [0, 2]);
		let config = config(root, universe, budget(0));
		let mut probe = Probe {
			config: &config,
			runner: &mut Fake { merges: Vec::new() },
			run: root.to_path_buf(),
			executable: String::new(),
			outcomes: HashMap::new(),
			minimized: Vec::new(),
			runs_left: 0,
			started: 0,
			pairs_skipped: 0,
		};
		let plan: Vec<Vec<usize>> = probe.plan().into_iter().map(|(_, set)| set).collect();
		// `sub` alone brings `base`, so the pair and prefix collapse into it.
		assert_eq!(plan, [vec![0], vec![1], vec![0, 2], vec![0, 1, 2]]);
	}

	#[test]
	fn real_merges_attribute_a_cross_directory_clash_to_its_pair() {
		let temp = tempfile::tempdir().unwrap();
		let fixture = crate::workshop_probe::tests::fixture(temp.path());
		let ids: Vec<String> = ["22", "33", "11"].map(String::from).to_vec();
		let pairs = crate::workshop_probe::tests::install_fixture(&fixture, &ids).unwrap();
		let content = &pairs[0].content_root;
		write_file(
			&content.join("22"),
			"common/static_modifiers/clash.txt",
			"cross_directory_clash = { global_tax_modifier = 0.4 }
",
		);
		write_file(
			&content.join("11"),
			"common/event_modifiers/clash.txt",
			"cross_directory_clash = { land_morale = 0.5 }
",
		);
		let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
		let (universe, missing) = resolve_universe(&ids, &pairs).unwrap();
		assert!(missing.is_empty());
		let mut budget = budget(50);
		budget.fail_on.insert("unsupported_input".into());
		let config = CombinationConfig {
			root: fixture.root.join("combinations"),
			game: fixture.game.clone(),
			base_data: fixture.base_data.clone(),
			universe,
			unavailable: Vec::new(),
			budget,
		};
		fs::create_dir_all(&config.root).unwrap();
		let before = fs::read(content.join("22/common/static_modifiers/clash.txt")).unwrap();
		let run = execute(&config, &mut CliRunner).unwrap();
		assert_eq!(
			fs::read(content.join("22/common/static_modifiers/clash.txt")).unwrap(),
			before
		);
		assert!(
			!run.join("0001/merged").exists(),
			"analysis must not commit"
		);
		let summary = summary(&config.root);
		assert_eq!(summary["unattributed"].as_array().unwrap().len(), 0);
		let buckets = summary["buckets"].as_array().unwrap();
		let clash = buckets
			.iter()
			.find(|b| b["signature"]["kind"] == "unsupported_input")
			.unwrap_or_else(|| panic!("no clash bucket: {summary:#}"));
		assert_eq!(clash["attribution"], "pair", "{summary:#}");
		assert_eq!(clash["culprits"][0], "fixture 22 (22)");
		assert_eq!(clash["culprits"][1], "fixture 11 (11)");
		assert_eq!(summary["singletons"].as_array().unwrap().len(), 3);
		assert_eq!(summary["completed"], 7, "{summary:#}");
		assert_eq!(summary["singletons"][0]["counts"]["unsupported_input"], 0);
	}

	#[test]
	fn review_text_and_errors_become_stable_signatures() {
		let case = Path::new("/probe/runs/run-x/0007");
		let observed = Observed {
			exit_code: Some(1),
			timed_out: false,
			elapsed_ms: 3,
			stdout: "Foch Merge Review\nstatus: blocked\nunits: 0\n  safe: 0\n  copy: 0\n  needs_user_choice: 0\n  unsupported_input: 0\n  engine_failure: 0\n  deferred: 0\nfatal errors:\n  - cannot read /probe/runs/run-x/0007/foch.toml\nreview units:\n".into(),
			stderr: String::new(),
		};
		let (completed, status, counts, signatures) = classify(&observed, case);
		assert!(completed);
		assert_eq!(status.as_deref(), Some("blocked"));
		assert_eq!(counts.unwrap().total, 0);
		assert_eq!(signatures[0].key, "fatal:cannot read <case>/foch.toml");
		let error = Observed {
			exit_code: Some(1),
			timed_out: false,
			elapsed_ms: 3,
			stdout: String::new(),
			stderr: "[foch] error: base data missing\n\n".into(),
		};
		let (completed, _, _, signatures) = classify(&error, case);
		assert!(!completed);
		assert_eq!(
			signatures[0].key,
			"error:exit Some(1): [foch] error: base data missing"
		);
	}
}
