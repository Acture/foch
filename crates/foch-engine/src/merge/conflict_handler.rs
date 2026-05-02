use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use foch_core::config::{
	DepOverride, ResolutionDecision, ResolutionEntry, ResolutionMap, compute_conflict_id,
};
use foch_core::model::HandlerResolutionRecord;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::merge::dag::ModDag;
use crate::merge::patch_merge::{PatchAddress, PatchConflict};

use super::tui_conflict_handler::InteractiveTuiHandler;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractiveMode {
	Cli,
	Tui,
	Auto,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InteractiveSettings {
	mode: InteractiveMode,
	config_path: Option<PathBuf>,
}

impl Default for InteractiveSettings {
	fn default() -> Self {
		Self {
			mode: InteractiveMode::Auto,
			config_path: None,
		}
	}
}

static INTERACTIVE_SETTINGS: OnceLock<Mutex<InteractiveSettings>> = OnceLock::new();

pub fn set_interactive_config_path(path: Option<PathBuf>) {
	set_interactive_mode_and_config(InteractiveMode::Auto, path);
}

pub fn set_interactive_mode_and_config(mode: InteractiveMode, config_path: Option<PathBuf>) {
	let mut slot = INTERACTIVE_SETTINGS
		.get_or_init(|| Mutex::new(InteractiveSettings::default()))
		.lock()
		.expect("interactive settings lock poisoned");
	*slot = InteractiveSettings { mode, config_path };
}

fn interactive_settings() -> InteractiveSettings {
	INTERACTIVE_SETTINGS
		.get_or_init(|| Mutex::new(InteractiveSettings::default()))
		.lock()
		.expect("interactive settings lock poisoned")
		.clone()
}

fn interactive_tui_available() -> bool {
	io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub trait ConflictHandler {
	fn on_conflict(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision;

	fn set_conflict_progress(&mut self, _current: usize, _total: usize) {}

	fn set_deferred_so_far(&mut self, _count: usize) {}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConflictDecision {
	/// Pick this mod's patch only; drop the others.
	PickMod(String),
	/// Pick this mod's patch and record a handler-specific report entry.
	PickModWithRecord {
		mod_id: String,
		record: HandlerResolutionRecord,
	},
	/// Use this external file's content (handled at materialize time).
	UseFile(PathBuf),
	/// Keep whatever already exists at output dir (handled at materialize time).
	KeepExisting,
	/// Defer — log to report, leave for later resolution.
	Defer,
	/// Abort the merge.
	Abort,
}

/// Default handler: always defer, reproducing the current behavior.
pub struct DeferHandler;

impl ConflictHandler for DeferHandler {
	fn on_conflict(&mut self, _: &str, _: &PatchAddress, _: &PatchConflict) -> ConflictDecision {
		ConflictDecision::Defer
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DepResolutionGraph {
	parents: HashMap<String, Vec<String>>,
}

impl DepResolutionGraph {
	pub(crate) fn from_mod_dag(mod_dag: &ModDag, dep_overrides: &[DepOverride]) -> Self {
		let ignored_edges: HashSet<(String, String)> = dep_overrides
			.iter()
			.map(|item| (item.mod_id.clone(), item.dep_id.clone()))
			.collect();
		let parents = mod_dag
			.topo()
			.iter()
			.map(|mod_id| {
				let child = mod_id.as_str().to_string();
				let parents = mod_dag
					.parents_of(mod_id)
					.iter()
					.filter(|parent| {
						!ignored_edges.contains(&(child.clone(), parent.as_str().to_string()))
					})
					.map(|parent| parent.as_str().to_string())
					.collect();
				(child, parents)
			})
			.collect();
		Self { parents }
	}

	#[cfg(test)]
	fn from_edges(edges: &[(&str, &str)]) -> Self {
		let mut parents: HashMap<String, Vec<String>> = HashMap::new();
		for (child, parent) in edges {
			parents
				.entry((*child).to_string())
				.or_default()
				.push((*parent).to_string());
			parents.entry((*parent).to_string()).or_default();
		}
		Self { parents }
	}

	fn direct_depends_on(&self, downstream: &str, upstream: &str) -> bool {
		self.parents
			.get(downstream)
			.is_some_and(|parents| parents.iter().any(|parent| parent == upstream))
	}

	fn depends_on(&self, downstream: &str, upstream: &str) -> bool {
		let mut seen = HashSet::new();
		let mut stack = self.parents.get(downstream).cloned().unwrap_or_default();
		while let Some(parent) = stack.pop() {
			if !seen.insert(parent.clone()) {
				continue;
			}
			if parent == upstream {
				return true;
			}
			if let Some(grandparents) = self.parents.get(&parent) {
				stack.extend(grandparents.iter().cloned());
			}
		}
		false
	}
}

pub(crate) struct DepImpliesResolutionHandler {
	current_file: PathBuf,
	dep_graph: DepResolutionGraph,
}

impl DepImpliesResolutionHandler {
	pub(crate) fn from_mod_dag(
		current_file: PathBuf,
		mod_dag: &ModDag,
		dep_overrides: &[DepOverride],
	) -> Self {
		Self::new(
			current_file,
			DepResolutionGraph::from_mod_dag(mod_dag, dep_overrides),
		)
	}

	pub(crate) fn new(current_file: PathBuf, dep_graph: DepResolutionGraph) -> Self {
		Self {
			current_file,
			dep_graph,
		}
	}

	fn conflict_mods(&self, conflict: &PatchConflict) -> Vec<String> {
		let mut seen = HashSet::new();
		conflict
			.patches
			.iter()
			.filter_map(|patch| {
				if seen.insert(patch.mod_id.clone()) {
					Some(patch.mod_id.clone())
				} else {
					None
				}
			})
			.collect()
	}

	fn cycle_pair(&self, mods: &[String]) -> Option<(String, String)> {
		for (index, left) in mods.iter().enumerate() {
			for right in mods.iter().skip(index + 1) {
				if self.dep_graph.depends_on(left, right) && self.dep_graph.depends_on(right, left)
				{
					return Some((left.clone(), right.clone()));
				}
			}
		}
		None
	}

	fn winner(&self, mods: &[String]) -> Option<String> {
		if mods.len() < 2 {
			return None;
		}
		if let Some((left, right)) = self.cycle_pair(mods) {
			eprintln!(
				"[foch] dep_implied skipped for {}: dependency cycle between {} and {}",
				self.current_file.display(),
				left,
				right
			);
			return None;
		}

		let winners: Vec<_> = mods
			.iter()
			.filter(|candidate| {
				let candidate = candidate.as_str();
				mods.iter().all(|other| {
					other.as_str() == candidate || self.dep_graph.depends_on(candidate, other)
				})
			})
			.cloned()
			.collect();
		if winners.len() == 1 {
			winners.into_iter().next()
		} else {
			None
		}
	}

	fn rationale(&self, winner: &str, mods: &[String]) -> String {
		for other in mods.iter().filter(|other| other.as_str() != winner) {
			if self.dep_graph.direct_depends_on(winner, other) {
				return format!("mod {winner} declares dep on {other}");
			}
		}
		for other in mods.iter().filter(|other| other.as_str() != winner) {
			if self.dep_graph.depends_on(winner, other) {
				return format!("mod {winner} transitively depends on {other}");
			}
		}
		format!("mod {winner} is downstream of all conflicting contributors")
	}
}

impl ConflictHandler for DepImpliesResolutionHandler {
	fn on_conflict(
		&mut self,
		_: &str,
		_: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision {
		let mods = self.conflict_mods(conflict);
		let Some(winner) = self.winner(&mods) else {
			return ConflictDecision::Defer;
		};
		let rationale = self.rationale(&winner, &mods);
		ConflictDecision::PickModWithRecord {
			mod_id: winner.clone(),
			record: HandlerResolutionRecord {
				path: self.current_file.to_string_lossy().replace('\\', "/"),
				action: "dep_implied".to_string(),
				source: Some(winner),
				rationale: Some(rationale),
			},
		}
	}
}

pub struct LookupHandler<'a> {
	pub map: &'a ResolutionMap,
	pub current_file: PathBuf,
	current_conflict_index: usize,
	total_conflicts: usize,
}

impl<'a> LookupHandler<'a> {
	pub fn new(map: &'a ResolutionMap, file: PathBuf) -> Self {
		Self::with_display_names(map, file, HashMap::new())
	}

	pub fn with_display_names(
		map: &'a ResolutionMap,
		file: PathBuf,
		_mod_displayname_lookup: HashMap<String, String>,
	) -> Self {
		Self {
			map,
			current_file: file,
			current_conflict_index: 1,
			total_conflicts: 1,
		}
	}
}

impl<'a> ConflictHandler for LookupHandler<'a> {
	fn on_conflict(
		&mut self,
		_path: &str,
		address: &PatchAddress,
		_conflict: &PatchConflict,
	) -> ConflictDecision {
		let address_path = address.path.join("/");
		let conflict_id = compute_conflict_id(&self.current_file, &address_path, &address.key);
		match self.map.lookup(&self.current_file, &conflict_id) {
			Some(ResolutionDecision::PreferMod(mod_id)) => {
				ConflictDecision::PickMod(mod_id.clone())
			}
			Some(ResolutionDecision::UseFile(path)) => ConflictDecision::UseFile(path.clone()),
			Some(ResolutionDecision::KeepExisting) => ConflictDecision::KeepExisting,
			None => ConflictDecision::Defer,
		}
	}

	fn set_conflict_progress(&mut self, current: usize, total: usize) {
		self.current_conflict_index = current;
		self.total_conflicts = total;
	}
}
pub trait ConfigWriter {
	fn append_resolution(&mut self, entry: ResolutionEntry) -> Result<(), Box<dyn Error>>;
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FilesystemConfigWriter {
	path: PathBuf,
}

impl FilesystemConfigWriter {
	pub fn new(path: PathBuf) -> Self {
		Self { path }
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	fn temporary_path(&self) -> PathBuf {
		let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
		let file_name = self
			.path
			.file_name()
			.and_then(|value| value.to_str())
			.unwrap_or("foch.toml");
		parent.join(format!(".{file_name}.{}.tmp", std::process::id()))
	}
}

impl ConfigWriter for FilesystemConfigWriter {
	fn append_resolution(&mut self, entry: ResolutionEntry) -> Result<(), Box<dyn Error>> {
		if let Some(parent) = self
			.path
			.parent()
			.filter(|parent| !parent.as_os_str().is_empty())
		{
			fs::create_dir_all(parent)?;
		}

		let content = match fs::read_to_string(&self.path) {
			Ok(content) => content,
			Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
			Err(err) => return Err(Box::new(err)),
		};
		if !content.trim().is_empty() {
			content.parse::<DocumentMut>()?;
		}

		let mut next_content = content;
		if !next_content.is_empty() && !next_content.ends_with('\n') {
			next_content.push('\n');
		}
		if !next_content.is_empty() && !next_content.ends_with("\n\n") {
			next_content.push('\n');
		}
		next_content.push_str(&render_resolution_entry(&entry));
		if !next_content.ends_with('\n') {
			next_content.push('\n');
		}

		let temp_path = self.temporary_path();
		fs::write(&temp_path, next_content.as_bytes())?;
		if let Err(err) = fs::rename(&temp_path, &self.path) {
			let _ = fs::remove_file(&temp_path);
			return Err(Box::new(err));
		}
		Ok(())
	}
}

pub struct InteractiveCliHandler {
	pub current_file: PathBuf,
	pub config_writer: Box<dyn ConfigWriter>,
	input: Box<dyn BufRead>,
	stderr: Box<dyn Write>,
	tty_available: Option<bool>,
	current_conflict_index: usize,
	total_conflicts: usize,
	deferred_so_far: usize,
}

impl InteractiveCliHandler {
	pub fn new(current_file: PathBuf, config_writer: Box<dyn ConfigWriter>) -> Self {
		Self {
			current_file,
			config_writer,
			input: Box::new(BufReader::new(io::stdin())),
			stderr: Box::new(io::stderr()),
			tty_available: None,
			current_conflict_index: 1,
			total_conflicts: 1,
			deferred_so_far: 0,
		}
	}

	#[cfg(test)]
	fn with_io(
		current_file: PathBuf,
		config_writer: Box<dyn ConfigWriter>,
		input: Box<dyn BufRead>,
		stderr: Box<dyn Write>,
		tty_available: bool,
	) -> Self {
		Self {
			current_file,
			config_writer,
			input,
			stderr,
			tty_available: Some(tty_available),
			current_conflict_index: 1,
			total_conflicts: 1,
			deferred_so_far: 0,
		}
	}

	fn stdin_stderr_are_tty(&self) -> bool {
		self.tty_available
			.unwrap_or_else(|| atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stderr))
	}

	fn write_conflict_summary(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
		conflict_id: &str,
	) {
		let address_path = address.path.join("/");
		let _ = writeln!(
			self.stderr,
			"[foch] unresolved structural merge conflict (conflict {}/{}) ({} deferred)",
			self.current_conflict_index, self.total_conflicts, self.deferred_so_far
		);
		let _ = writeln!(self.stderr, "  file: {}", self.current_file.display());
		let _ = writeln!(self.stderr, "  path: {path}");
		let _ = writeln!(self.stderr, "  address: {address_path}/{}", address.key);
		let _ = writeln!(self.stderr, "  conflict_id: {conflict_id}");
		let _ = writeln!(self.stderr, "  reason: {}", conflict.reason);
		let _ = writeln!(self.stderr, "  candidates:");
		for (index, candidate) in conflict.patches.iter().enumerate() {
			let _ = writeln!(
				self.stderr,
				"    [{}] {} (precedence {})",
				index + 1,
				candidate.mod_id,
				candidate.precedence
			);
			self.write_candidate_patch(candidate);
		}
	}

	fn write_candidate_patch(&mut self, candidate: &crate::merge::patch_merge::AttributedPatch) {
		let rendered = format!("{:#?}", candidate.patch);
		let lines: Vec<&str> = rendered.lines().collect();
		for line in lines.iter().take(20) {
			let _ = writeln!(self.stderr, "      {line}");
		}
		let remaining = lines.len().saturating_sub(20);
		if remaining > 0 {
			let _ = writeln!(self.stderr, "      ... ({remaining} more lines)");
		}
	}

	fn write_prompt(&mut self, conflict: &PatchConflict) {
		let mut choices = conflict
			.patches
			.iter()
			.enumerate()
			.map(|(index, patch)| format!("[{}] {}", index + 1, patch.mod_id))
			.collect::<Vec<_>>();
		choices.extend([
			"[d] defer".to_string(),
			"[s] use file path".to_string(),
			"[k] keep existing".to_string(),
			"[q] abort".to_string(),
		]);
		let _ = write!(self.stderr, "{}\nchoice> ", choices.join(" / "));
		let _ = self.stderr.flush();
	}

	fn read_trimmed_line(&mut self) -> Option<String> {
		let mut line = String::new();
		match self.input.read_line(&mut line) {
			Ok(0) => None,
			Ok(_) => Some(line.trim().to_string()),
			Err(err) => {
				let _ = writeln!(
					self.stderr,
					"[foch] failed to read interactive choice: {err}"
				);
				None
			}
		}
	}

	fn prompt_for_external_path(&mut self) -> Option<PathBuf> {
		let _ = write!(self.stderr, "path> ");
		let _ = self.stderr.flush();
		let value = self.read_trimmed_line()?;
		if value.is_empty() {
			None
		} else {
			Some(PathBuf::from(value))
		}
	}

	fn persist_decision(
		&mut self,
		address: &PatchAddress,
		decision: ConflictDecision,
	) -> ConflictDecision {
		let Some(entry) = resolution_entry_for_decision(&self.current_file, address, &decision)
		else {
			return decision;
		};
		match self.config_writer.append_resolution(entry) {
			Ok(()) => decision,
			Err(err) => {
				let _ = writeln!(
					self.stderr,
					"[foch] failed to persist interactive resolution: {err}"
				);
				ConflictDecision::Abort
			}
		}
	}
}

impl ConflictHandler for InteractiveCliHandler {
	fn on_conflict(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision {
		if !self.stdin_stderr_are_tty() {
			let _ = writeln!(
				self.stderr,
				"[foch] interactive mode could not be entered because stdin/stderr is not a TTY; downgrading to defer"
			);
			return ConflictDecision::Defer;
		}

		let address_path = address.path.join("/");
		let conflict_id = compute_conflict_id(&self.current_file, &address_path, &address.key);
		self.write_conflict_summary(path, address, conflict, &conflict_id);

		for attempt in 1..=3 {
			self.write_prompt(conflict);
			let Some(choice) = self.read_trimmed_line() else {
				return ConflictDecision::Defer;
			};
			let choice = choice.to_ascii_lowercase();
			match choice.as_str() {
				"d" | "defer" => return ConflictDecision::Defer,
				"q" | "quit" | "abort" => return ConflictDecision::Abort,
				"k" | "keep" => {
					return self.persist_decision(address, ConflictDecision::KeepExisting);
				}
				"s" | "file" | "use-file" => {
					if let Some(path) = self.prompt_for_external_path() {
						return self.persist_decision(address, ConflictDecision::UseFile(path));
					}
				}
				_ => {
					if let Ok(index) = choice.parse::<usize>()
						&& let Some(candidate) = index
							.checked_sub(1)
							.and_then(|index| conflict.patches.get(index))
					{
						return self.persist_decision(
							address,
							ConflictDecision::PickMod(candidate.mod_id.clone()),
						);
					}
				}
			}
			if attempt < 3 {
				let _ = writeln!(self.stderr, "[foch] invalid choice; please try again");
			}
		}

		let _ = writeln!(
			self.stderr,
			"[foch] invalid choice limit reached; deferring conflict"
		);
		ConflictDecision::Defer
	}

	fn set_conflict_progress(&mut self, current: usize, total: usize) {
		self.current_conflict_index = current;
		self.total_conflicts = total;
	}

	fn set_deferred_so_far(&mut self, count: usize) {
		self.deferred_so_far = count;
	}
}

/// Chain combinator: returns the second handler's decision when the first defers.
pub struct ChainHandler<H1: ConflictHandler, H2: ConflictHandler> {
	pub first: H1,
	pub second: H2,
}

impl<H1: ConflictHandler, H2: ConflictHandler> ConflictHandler for ChainHandler<H1, H2> {
	fn on_conflict(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision {
		match self.first.on_conflict(path, address, conflict) {
			ConflictDecision::Defer => self.second.on_conflict(path, address, conflict),
			other => other,
		}
	}

	fn set_conflict_progress(&mut self, current: usize, total: usize) {
		self.first.set_conflict_progress(current, total);
		self.second.set_conflict_progress(current, total);
	}

	fn set_deferred_so_far(&mut self, count: usize) {
		self.first.set_deferred_so_far(count);
		self.second.set_deferred_so_far(count);
	}
}

pub(crate) fn resolution_entry_for_decision(
	current_file: &Path,
	address: &PatchAddress,
	decision: &ConflictDecision,
) -> Option<ResolutionEntry> {
	let address_path = address.path.join("/");
	let conflict_id = compute_conflict_id(current_file, &address_path, &address.key);
	match decision {
		ConflictDecision::PickMod(mod_id) | ConflictDecision::PickModWithRecord { mod_id, .. } => {
			Some(ResolutionEntry {
				file: None,
				conflict_id: Some(conflict_id),
				mod_id: None,
				prefer_mod: Some(mod_id.clone()),
				use_file: None,
				keep_existing: None,
				priority_boost: None,
			})
		}
		ConflictDecision::UseFile(path) => Some(ResolutionEntry {
			file: None,
			conflict_id: Some(conflict_id),
			mod_id: None,
			prefer_mod: None,
			use_file: Some(path.clone()),
			keep_existing: None,
			priority_boost: None,
		}),
		ConflictDecision::KeepExisting => Some(ResolutionEntry {
			file: Some(current_file.to_path_buf()),
			conflict_id: None,
			mod_id: None,
			prefer_mod: None,
			use_file: None,
			keep_existing: Some(true),
			priority_boost: None,
		}),
		ConflictDecision::Defer | ConflictDecision::Abort => None,
	}
}

/// Outcome from prompting the user about a single surviving conflict.
#[derive(Debug, Clone)]
pub enum PromptOutcomeKind {
	Picked(ResolutionDecision),
	Deferred,
}

#[derive(Debug, Clone)]
pub struct PromptOutcome {
	pub conflict_id: String,
	pub kind: PromptOutcomeKind,
}

/// Result of running the post-pass interactive resolver.
#[derive(Debug, Clone, Default)]
pub struct PromptSurvivorsResult {
	pub outcomes: Vec<PromptOutcome>,
	pub aborted: bool,
}

/// Returns true when interactive prompting is configured and feasible.
pub fn interactive_prompt_enabled() -> bool {
	let settings = interactive_settings();
	settings.config_path.is_some()
		&& match settings.mode {
			InteractiveMode::Auto => interactive_tui_available(),
			InteractiveMode::Tui => interactive_tui_available(),
			InteractiveMode::Cli => io::stdin().is_terminal() && io::stderr().is_terminal(),
		}
}

/// Prompts the user interactively for each surviving conflict (the post-pass
/// path: only invoked once the merge engine has finished and downstream
/// overrides have already pruned transient conflicts). Persists every Picked
/// decision to foch.toml as a side effect.
///
/// `survivors` should be the list of `(address, conflict)` extracted from
/// `PatchResolution::Conflict` survivors. The returned outcomes carry the
/// resolution-map decision the caller should fold into the in-memory map
/// before re-running the merge engine. If the user aborts, `aborted` is set
/// and any outcomes already collected are still returned.
pub fn prompt_survivors_and_persist(
	target_path: &Path,
	survivors: &[(PatchAddress, PatchConflict)],
	mod_displayname_lookup: &HashMap<String, String>,
) -> PromptSurvivorsResult {
	let settings = interactive_settings();
	let Some(config_path) = settings.config_path.clone() else {
		return PromptSurvivorsResult::default();
	};
	let mode = match settings.mode {
		InteractiveMode::Auto if interactive_tui_available() => InteractiveMode::Tui,
		InteractiveMode::Auto => InteractiveMode::Cli,
		other => other,
	};

	let total = survivors.len();
	let mut deferred_so_far = 0usize;
	let mut result = PromptSurvivorsResult::default();
	for (idx, (address, conflict)) in survivors.iter().enumerate() {
		let current = idx + 1;
		let conflict_id = compute_conflict_id(target_path, &address.path.join("/"), &address.key);
		let decision = match mode {
			InteractiveMode::Tui => {
				let mut handler = InteractiveTuiHandler::new(
					target_path.to_path_buf(),
					Box::new(FilesystemConfigWriter::new(config_path.clone())),
					mod_displayname_lookup.clone(),
					current,
					total,
					deferred_so_far,
				);
				handler.on_conflict("", address, conflict)
			}
			InteractiveMode::Cli | InteractiveMode::Auto => {
				let mut handler = InteractiveCliHandler::new(
					target_path.to_path_buf(),
					Box::new(FilesystemConfigWriter::new(config_path.clone())),
				);
				handler.set_conflict_progress(current, total);
				handler.set_deferred_so_far(deferred_so_far);
				handler.on_conflict("", address, conflict)
			}
		};
		match decision {
			ConflictDecision::PickMod(mod_id)
			| ConflictDecision::PickModWithRecord { mod_id, .. } => {
				result.outcomes.push(PromptOutcome {
					conflict_id,
					kind: PromptOutcomeKind::Picked(ResolutionDecision::PreferMod(mod_id)),
				});
			}
			ConflictDecision::UseFile(path) => result.outcomes.push(PromptOutcome {
				conflict_id,
				kind: PromptOutcomeKind::Picked(ResolutionDecision::UseFile(path)),
			}),
			ConflictDecision::KeepExisting => result.outcomes.push(PromptOutcome {
				conflict_id,
				kind: PromptOutcomeKind::Picked(ResolutionDecision::KeepExisting),
			}),
			ConflictDecision::Defer => {
				result.outcomes.push(PromptOutcome {
					conflict_id,
					kind: PromptOutcomeKind::Deferred,
				});
				deferred_so_far += 1;
			}
			ConflictDecision::Abort => {
				result.aborted = true;
				break;
			}
		}
	}
	result
}

fn render_resolution_entry(entry: &ResolutionEntry) -> String {
	let mut table = Table::new();
	if let Some(file) = &entry.file {
		table["file"] = value(path_to_toml_string(file));
	}
	if let Some(conflict_id) = &entry.conflict_id {
		table["conflict_id"] = value(conflict_id.clone());
	}
	if let Some(mod_id) = &entry.mod_id {
		table["mod"] = value(mod_id.clone());
	}
	if let Some(prefer_mod) = &entry.prefer_mod {
		table["prefer_mod"] = value(prefer_mod.clone());
	}
	if let Some(use_file) = &entry.use_file {
		table["use_file"] = value(path_to_toml_string(use_file));
	}
	if let Some(keep_existing) = entry.keep_existing {
		table["keep_existing"] = value(keep_existing);
	}
	if let Some(priority_boost) = entry.priority_boost {
		table["priority_boost"] = value(i64::from(priority_boost));
	}

	let mut resolutions = ArrayOfTables::new();
	resolutions.push(table);
	let mut doc = DocumentMut::new();
	doc["resolutions"] = Item::ArrayOfTables(resolutions);
	doc.to_string()
}

fn path_to_toml_string(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;
	use std::collections::HashMap;
	use std::io::Cursor;
	use std::path::PathBuf;
	use std::rc::Rc;
	use std::time::{SystemTime, UNIX_EPOCH};

	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::ModCandidate;
	use foch_language::analyzer::parser::{AstValue, ScalarValue, Span, SpanRange};

	use super::*;
	use crate::merge::patch::ClausewitzPatch;
	use crate::merge::patch_merge::AttributedPatch;

	fn address() -> PatchAddress {
		PatchAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "id".to_string(),
		}
	}

	fn conflict() -> PatchConflict {
		PatchConflict {
			patches: Vec::new(),
			reason: "test conflict".to_string(),
		}
	}

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
			end: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
		}
	}

	fn scalar(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Identifier(value.to_string()),
			span: span(),
		}
	}

	fn attributed_patch(mod_id: &str, precedence: usize, value: &str) -> AttributedPatch {
		AttributedPatch {
			mod_id: mod_id.to_string(),
			precedence,
			patch: ClausewitzPatch::SetValue {
				path: vec!["root".to_string()],
				key: "id".to_string(),
				old_value: scalar("old"),
				new_value: scalar(value),
			},
		}
	}

	fn conflict_with_patches() -> PatchConflict {
		conflict_with_mods(&[("mod_a", 1, "alpha"), ("mod_b", 2, "beta")])
	}

	fn conflict_with_mods(mods: &[(&str, usize, &str)]) -> PatchConflict {
		PatchConflict {
			patches: mods
				.iter()
				.map(|(mod_id, precedence, value)| attributed_patch(mod_id, *precedence, value))
				.collect(),
			reason: "mods disagree".to_string(),
		}
	}

	fn dep_handler(edges: &[(&str, &str)]) -> DepImpliesResolutionHandler {
		DepImpliesResolutionHandler::new(
			PathBuf::from("common/ideas/dep.txt"),
			DepResolutionGraph::from_edges(edges),
		)
	}

	fn assert_dep_pick(decision: ConflictDecision, expected_mod: &str, expected_rationale: &str) {
		match decision {
			ConflictDecision::PickModWithRecord { mod_id, record } => {
				assert_eq!(mod_id, expected_mod);
				assert_eq!(record.path, "common/ideas/dep.txt");
				assert_eq!(record.action, "dep_implied");
				assert_eq!(record.source.as_deref(), Some(expected_mod));
				assert_eq!(record.rationale.as_deref(), Some(expected_rationale));
			}
			other => panic!("expected dep-implied pick, got {other:?}"),
		}
	}

	fn mod_candidate(mod_id: &str, name: &str, dependencies: &[&str]) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaylistEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: dependencies.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	#[derive(Clone, Default)]
	struct RecordingWriter {
		entries: Rc<RefCell<Vec<ResolutionEntry>>>,
	}

	impl RecordingWriter {
		fn new() -> (Self, Rc<RefCell<Vec<ResolutionEntry>>>) {
			let entries = Rc::new(RefCell::new(Vec::new()));
			(
				Self {
					entries: Rc::clone(&entries),
				},
				entries,
			)
		}
	}

	impl ConfigWriter for RecordingWriter {
		fn append_resolution(&mut self, entry: ResolutionEntry) -> Result<(), Box<dyn Error>> {
			self.entries.borrow_mut().push(entry);
			Ok(())
		}
	}

	fn handler_with_input(
		current_file: PathBuf,
		input: &str,
		tty_available: bool,
	) -> (InteractiveCliHandler, Rc<RefCell<Vec<ResolutionEntry>>>) {
		let (writer, entries) = RecordingWriter::new();
		let handler = InteractiveCliHandler::with_io(
			current_file,
			Box::new(writer),
			Box::new(Cursor::new(input.as_bytes().to_vec())),
			Box::new(io::sink()),
			tty_available,
		);
		(handler, entries)
	}

	#[test]
	fn lookup_handler_returns_pick_mod_when_resolution_map_has_entry() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = HashMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod-a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = LookupHandler::new(&map, current_file);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict());

		assert_eq!(decision, ConflictDecision::PickMod("mod-a".to_string()));
	}

	#[test]
	fn lookup_handler_returns_defer_on_miss() {
		let map = ResolutionMap::default();
		let mut handler = LookupHandler::new(&map, PathBuf::from("events/PirateEvents.txt"));

		let decision = handler.on_conflict("root/event/id", &address(), &conflict());

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn lookup_handler_chained_with_defer_uses_resolution_then_defers() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = HashMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod-a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, current_file),
			second: DeferHandler,
		};
		let miss = PatchAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "other".to_string(),
		};

		let resolved = handler.on_conflict("root/event/id", &address(), &conflict());
		let deferred = handler.on_conflict("root/event/other", &miss, &conflict());

		assert_eq!(resolved, ConflictDecision::PickMod("mod-a".to_string()));
		assert_eq!(deferred, ConflictDecision::Defer);
	}

	#[test]
	fn dep_implies_resolution_picks_two_mod_downstream() {
		let mut handler = dep_handler(&[("mod_a", "mod_b")]);

		let decision = handler.on_conflict("root/id", &address(), &conflict_with_patches());

		assert_dep_pick(decision, "mod_a", "mod mod_a declares dep on mod_b");
	}

	#[test]
	fn dep_implies_resolution_picks_downstream_over_two_upstreams() {
		let mut handler = dep_handler(&[("mod_a", "mod_b"), ("mod_a", "mod_c")]);
		let conflict = conflict_with_mods(&[
			("mod_a", 3, "alpha"),
			("mod_b", 1, "beta"),
			("mod_c", 2, "gamma"),
		]);

		let decision = handler.on_conflict("root/id", &address(), &conflict);

		assert_dep_pick(decision, "mod_a", "mod mod_a declares dep on mod_b");
	}

	#[test]
	fn dep_implies_resolution_picks_most_downstream_in_chain() {
		let mut handler =
			dep_handler(&[("mod_a", "mod_b"), ("mod_a", "mod_c"), ("mod_b", "mod_c")]);
		let conflict = conflict_with_mods(&[
			("mod_a", 3, "alpha"),
			("mod_b", 2, "beta"),
			("mod_c", 1, "gamma"),
		]);

		let decision = handler.on_conflict("root/id", &address(), &conflict);

		assert_dep_pick(decision, "mod_a", "mod mod_a declares dep on mod_b");
	}

	#[test]
	fn dep_implies_resolution_defers_independent_mods() {
		let mut handler = dep_handler(&[]);

		let decision = handler.on_conflict("root/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn dep_implies_resolution_defers_when_any_contributor_is_independent() {
		let mut handler = dep_handler(&[("mod_a", "mod_b")]);
		let conflict = conflict_with_mods(&[
			("mod_a", 3, "alpha"),
			("mod_b", 1, "beta"),
			("mod_c", 2, "gamma"),
		]);

		let decision = handler.on_conflict("root/id", &address(), &conflict);

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn dep_implies_resolution_defers_on_cycle() {
		let mut handler = dep_handler(&[("mod_a", "mod_b"), ("mod_b", "mod_a")]);

		let decision = handler.on_conflict("root/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn dep_implies_resolution_respects_dep_overrides() {
		let mods = vec![
			mod_candidate("mod_b", "Mod B", &[]),
			mod_candidate("mod_a", "Mod A", &["Mod B"]),
		];
		let (dag, diagnostics) = crate::merge::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty());
		let graph = DepResolutionGraph::from_mod_dag(
			&dag,
			&[foch_core::config::DepOverride::new("mod_a", "mod_b")],
		);
		let mut handler =
			DepImpliesResolutionHandler::new(PathBuf::from("common/ideas/dep.txt"), graph);

		let decision = handler.on_conflict("root/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn interactive_handler_returns_defer_on_non_tty() {
		let (mut handler, entries) =
			handler_with_input(PathBuf::from("events/PirateEvents.txt"), "1\n", false);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::Defer);
		assert!(entries.borrow().is_empty());
	}

	#[test]
	fn interactive_handler_returns_pick_mod_on_user_choice() {
		let (mut handler, _) =
			handler_with_input(PathBuf::from("events/PirateEvents.txt"), "2\n", true);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::PickMod("mod_b".to_string()));
	}

	#[test]
	fn interactive_handler_writes_resolution_to_config_writer() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let (mut handler, entries) = handler_with_input(current_file.clone(), "1\n", true);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::PickMod("mod_a".to_string()));
		let entries = entries.borrow();
		assert_eq!(entries.len(), 1);
		assert_eq!(
			entries[0].conflict_id,
			Some(compute_conflict_id(&current_file, "root/event", "id"))
		);
		assert_eq!(entries[0].prefer_mod.as_deref(), Some("mod_a"));
	}

	#[test]
	fn interactive_handler_returns_keep_existing_on_user_choice_k() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let (mut handler, entries) = handler_with_input(current_file.clone(), "k\n", true);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::KeepExisting);
		let entries = entries.borrow();
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].file.as_ref(), Some(&current_file));
		assert_eq!(entries[0].keep_existing, Some(true));
		assert_eq!(entries[0].conflict_id, None);
	}

	#[test]
	fn interactive_handler_invalid_input_eventually_defers() {
		let (mut handler, entries) =
			handler_with_input(PathBuf::from("events/PirateEvents.txt"), "x\ny\n0\n", true);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::Defer);
		assert!(entries.borrow().is_empty());
	}

	#[test]
	fn interactive_handler_writes_use_file_resolution_to_config_writer() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let (mut handler, entries) = handler_with_input(
			current_file.clone(),
			"s\nresolutions/PirateEvents.txt\n",
			true,
		);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(
			decision,
			ConflictDecision::UseFile(PathBuf::from("resolutions/PirateEvents.txt"))
		);
		let entries = entries.borrow();
		assert_eq!(entries.len(), 1);
		assert_eq!(
			entries[0].conflict_id,
			Some(compute_conflict_id(&current_file, "root/event", "id"))
		);
		assert_eq!(
			entries[0].use_file.as_ref(),
			Some(&PathBuf::from("resolutions/PirateEvents.txt"))
		);
	}

	#[test]
	fn merge_command_with_interactive_handler_chains_handlers_correctly() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = HashMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod_a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let (interactive, entries) = handler_with_input(current_file.clone(), "2\n", true);
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, current_file),
			second: ChainHandler {
				first: interactive,
				second: DeferHandler,
			},
		};

		let decision = handler.on_conflict("root/event/id", &address(), &conflict_with_patches());

		assert_eq!(decision, ConflictDecision::PickMod("mod_a".to_string()));
		assert!(
			entries.borrow().is_empty(),
			"lookup hit should not invoke interactive writer"
		);
	}

	#[test]
	fn filesystem_config_writer_appends_resolution_without_dropping_existing_content() {
		let root = project_test_dir("filesystem_config_writer_appends_resolution");
		let path = root.join("foch.toml");
		fs::create_dir_all(&root).expect("create test dir");
		fs::write(
			&path,
			r#"# keep this comment

[[overrides]]
mod = "a"
dep = "b"
"#,
		)
		.expect("write config");
		let mut writer = FilesystemConfigWriter::new(path.clone());

		writer
			.append_resolution(ResolutionEntry {
				file: None,
				conflict_id: Some("abc12345".to_string()),
				mod_id: None,
				prefer_mod: Some("mod_a".to_string()),
				use_file: None,
				keep_existing: None,
				priority_boost: None,
			})
			.expect("append resolution");

		let content = fs::read_to_string(&path).expect("read config");
		assert!(content.contains("# keep this comment"));
		assert!(content.contains("[[overrides]]"));
		assert!(content.contains("[[resolutions]]"));
		assert!(content.contains(r#"conflict_id = "abc12345""#));
		assert!(content.contains(r#"prefer_mod = "mod_a""#));
		let parsed = foch_core::config::FochConfig::from_toml_str(&content).expect("parse config");
		assert_eq!(parsed.resolutions.len(), 1);
	}

	fn project_test_dir(name: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock after epoch")
			.as_nanos();
		std::env::current_dir()
			.expect("current dir")
			.join("target")
			.join("foch-engine-tests")
			.join(format!("{name}-{}-{nanos}", std::process::id()))
	}
}
