use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};

use foch::model::HandlerResolutionRecord;
use foch::project::{
	DepOverride, ResolutionDecision, ResolutionEntry, ResolutionMap, compute_conflict_id,
};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::merge::conflict_view::ConflictView;
use crate::merge::dag::ModDag;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConflictViewRequirement {
	/// The handler always returns an unrecorded defer decision.
	DeferWithoutView,
	/// The handler only needs stable identity and contributor metadata.
	Metadata,
	/// The handler needs rendered vanilla/candidate snippets.
	Full,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictMetadataCandidate {
	pub mod_id: String,
	pub precedence: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictMetadataView {
	pub file_path: PathBuf,
	pub address_path: Vec<String>,
	pub address_key: String,
	pub conflict_id: String,
	pub reason: String,
	pub candidates: Vec<ConflictMetadataCandidate>,
}

impl From<&ConflictView> for ConflictMetadataView {
	fn from(view: &ConflictView) -> Self {
		Self {
			file_path: view.file_path.clone(),
			address_path: view.address_path.clone(),
			address_key: view.address_key.clone(),
			conflict_id: view.conflict_id.clone(),
			reason: view.reason.clone(),
			candidates: view
				.candidates
				.iter()
				.map(|candidate| ConflictMetadataCandidate {
					mod_id: candidate.mod_id.clone(),
					precedence: candidate.precedence,
				})
				.collect(),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataConflictDecision {
	Decision(ConflictDecision),
	NeedsFullView,
}

impl From<ConflictDecision> for MetadataConflictDecision {
	fn from(decision: ConflictDecision) -> Self {
		Self::Decision(decision)
	}
}

pub trait ConflictHandler {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision;

	/// Declare the cheapest view that preserves this handler's behavior.
	///
	/// External handlers default to `Full`; opting into `Metadata` also requires
	/// implementing `on_conflict_metadata`.
	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		ConflictViewRequirement::Full
	}

	fn on_conflict_metadata(&mut self, _view: &ConflictMetadataView) -> MetadataConflictDecision {
		panic!("metadata-capable conflict handler must implement on_conflict_metadata")
	}

	fn set_conflict_progress(&mut self, _current: usize, _total: usize) {}

	fn set_deferred_so_far(&mut self, _count: usize) {}
}

fn unique_candidate_index(view: &ConflictMetadataView, mod_id: &str) -> Option<usize> {
	let mut matches = view
		.candidates
		.iter()
		.enumerate()
		.filter(|(_, candidate)| candidate.mod_id == mod_id)
		.map(|(index, _)| index);
	let candidate = matches.next()?;
	matches.next().is_none().then_some(candidate)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConflictDecision {
	/// Pick one exact candidate from the `ConflictView` passed to the handler.
	PickCandidate {
		candidate: usize,
		record: Option<HandlerResolutionRecord>,
	},
	/// Use this external file's content (handled at materialize time).
	UseFile(PathBuf),
	/// Use bytes frozen from a configured external file during preparation.
	UseFrozenFile(PathBuf),
	/// Keep whatever already exists at output dir (handled at materialize time).
	KeepExisting,
	/// Defer — log to report, leave for later resolution, optionally recording a handler-specific report entry.
	Defer {
		record: Option<HandlerResolutionRecord>,
	},
	/// Abort the merge.
	Abort,
}

/// Default handler: always defer, reproducing the current behavior.
pub struct DeferHandler;

impl ConflictHandler for DeferHandler {
	fn on_conflict(&mut self, _: &ConflictView) -> ConflictDecision {
		ConflictDecision::Defer { record: None }
	}

	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		ConflictViewRequirement::DeferWithoutView
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

/// Single-threaded only: holds &mut self per-conflict via the ConflictHandler trait.
/// The merge engine drives this serially; do NOT share across threads.
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

	fn conflict_mods(&self, view: &ConflictMetadataView) -> Vec<String> {
		let mut seen = HashSet::new();
		view.candidates
			.iter()
			.filter_map(|candidate| {
				if seen.insert(candidate.mod_id.clone()) {
					Some(candidate.mod_id.clone())
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

	fn decide(&mut self, view: &ConflictMetadataView) -> ConflictDecision {
		let mods = self.conflict_mods(view);
		let Some(winner) = self.winner(&mods) else {
			return ConflictDecision::Defer { record: None };
		};
		let Some(candidate) = unique_candidate_index(view, &winner) else {
			return ConflictDecision::Defer { record: None };
		};
		let rationale = self.rationale(&winner, &mods);
		ConflictDecision::PickCandidate {
			candidate,
			record: Some(HandlerResolutionRecord {
				path: view.file_path.to_string_lossy().replace('\\', "/"),
				action: "dep_implied".to_string(),
				source: Some(winner),
				rationale: Some(rationale),
			}),
		}
	}
}

impl ConflictHandler for DepImpliesResolutionHandler {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		self.decide(&ConflictMetadataView::from(view))
	}

	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		ConflictViewRequirement::Metadata
	}

	fn on_conflict_metadata(&mut self, view: &ConflictMetadataView) -> MetadataConflictDecision {
		self.decide(view).into()
	}
}

pub(crate) struct PriorityBoostResolutionHandler<'a> {
	_current_file: PathBuf,
	boosts: &'a BTreeMap<String, i32>,
}

impl<'a> PriorityBoostResolutionHandler<'a> {
	pub(crate) fn new(current_file: PathBuf, boosts: &'a BTreeMap<String, i32>) -> Self {
		Self {
			_current_file: current_file,
			boosts,
		}
	}

	fn winner(&self, view: &ConflictMetadataView) -> Option<(usize, String, usize)> {
		let (winner_index, winner) = view.candidates.iter().enumerate().max_by(
			|(left_index, left), (right_index, right)| {
				left.precedence
					.cmp(&right.precedence)
					.then_with(|| left.mod_id.cmp(&right.mod_id))
					.then_with(|| left_index.cmp(right_index))
			},
		)?;
		if self.boosts.get(&winner.mod_id).copied().unwrap_or(0) == 0 {
			return None;
		}
		let tied_winners = view
			.candidates
			.iter()
			.filter(|candidate| candidate.precedence == winner.precedence)
			.count();
		if tied_winners != 1 {
			return None;
		}
		Some((winner_index, winner.mod_id.clone(), winner.precedence))
	}

	fn decide(&mut self, view: &ConflictMetadataView) -> ConflictDecision {
		let Some((candidate, winner, precedence)) = self.winner(view) else {
			return ConflictDecision::Defer { record: None };
		};
		let mod_ids: Vec<&str> = view
			.candidates
			.iter()
			.map(|candidate| candidate.mod_id.as_str())
			.collect();
		ConflictDecision::PickCandidate {
			candidate,
			record: Some(HandlerResolutionRecord {
				path: view.file_path.to_string_lossy().replace('\\', "/"),
				action: "priority_boost".to_string(),
				source: Some(winner.clone()),
				rationale: Some(format!(
					"priority_boost made `{winner}` the unique highest-precedence contributor ({precedence}) among [{}]",
					mod_ids.join(", ")
				)),
			}),
		}
	}
}

impl<'a> ConflictHandler for PriorityBoostResolutionHandler<'a> {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		self.decide(&ConflictMetadataView::from(view))
	}

	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		ConflictViewRequirement::Metadata
	}

	fn on_conflict_metadata(&mut self, view: &ConflictMetadataView) -> MetadataConflictDecision {
		self.decide(view).into()
	}
}

/// Single-threaded only: holds &mut self per-conflict via current_conflict_index/total_conflicts.
/// The merge engine drives this serially; do NOT share across threads.
pub struct LookupHandler<'a> {
	pub map: &'a ResolutionMap,
	pub _current_file: PathBuf,
	current_conflict_index: usize,
	total_conflicts: usize,
}

impl<'a> LookupHandler<'a> {
	#[cfg(test)]
	pub(crate) fn new(map: &'a ResolutionMap, file: PathBuf) -> Self {
		Self::with_display_names(map, file, HashMap::new())
	}

	pub(crate) fn with_display_names(
		map: &'a ResolutionMap,
		file: PathBuf,
		_mod_displayname_lookup: HashMap<String, String>,
	) -> Self {
		Self {
			map,
			_current_file: file,
			current_conflict_index: 1,
			total_conflicts: 1,
		}
	}

	fn lookup_file<'view>(&'view self, view: &'view ConflictMetadataView) -> &'view Path {
		if self._current_file.as_os_str().is_empty() {
			&view.file_path
		} else {
			&self._current_file
		}
	}

	fn matching_resolution<'view>(
		&'view self,
		view: &'view ConflictMetadataView,
	) -> Option<&'view ResolutionDecision> {
		let address_path = view.address_path.join("/");
		let lookup_file = self.lookup_file(view);
		let leaf_address = if address_path.is_empty() {
			view.address_key.clone()
		} else {
			format!("{address_path}/{}", view.address_key)
		};
		let view_address_conflict_id =
			compute_conflict_id(&view.file_path, &address_path, &view.address_key);
		let address_conflict_id = (view.conflict_id == view_address_conflict_id)
			.then(|| compute_conflict_id(lookup_file, &address_path, &view.address_key));
		self.map
			.by_conflict_id
			.get(&view.conflict_id)
			.or_else(|| {
				address_conflict_id
					.as_ref()
					.and_then(|conflict_id| self.map.by_conflict_id.get(conflict_id))
			})
			.or_else(|| self.map.by_file.get(lookup_file))
			.or_else(|| {
				self.map
					.pattern_rules
					.iter()
					.find(|rule| rule.matches(lookup_file, &leaf_address))
					.map(|rule| &rule.decision)
			})
	}

	fn decide_metadata(&mut self, view: &ConflictMetadataView) -> ConflictDecision {
		match self.matching_resolution(view) {
			Some(ResolutionDecision::PreferCandidate(candidate)) => view
				.candidates
				.get(*candidate)
				.map(|_| ConflictDecision::PickCandidate {
					candidate: *candidate,
					record: None,
				})
				.unwrap_or(ConflictDecision::Defer { record: None }),
			Some(ResolutionDecision::PreferMod(mod_id)) => unique_candidate_index(view, mod_id)
				.map(|candidate| ConflictDecision::PickCandidate {
					candidate,
					record: None,
				})
				.unwrap_or(ConflictDecision::Defer { record: None }),
			Some(ResolutionDecision::UseFile(path)) => {
				ConflictDecision::UseFrozenFile(path.clone())
			}
			Some(ResolutionDecision::UseLiveFile(path)) => ConflictDecision::UseFile(path.clone()),
			Some(ResolutionDecision::KeepExisting) => ConflictDecision::KeepExisting,
			Some(ResolutionDecision::Handler(_)) => {
				unreachable!("named handlers require a full conflict view")
			}
			None => {
				log_cwt_suggestion_on_miss(
					self.lookup_file(view),
					&view.address_path,
					&view.address_key,
				);
				ConflictDecision::Defer { record: None }
			}
		}
	}
}

fn log_cwt_suggestion_on_miss(current_file: &Path, address_path: &[String], address_key: &str) {
	let ast_path = if address_path.is_empty() {
		vec![address_key]
	} else {
		address_path.iter().map(String::as_str).collect::<Vec<_>>()
	};
	let Some(suggestion) =
		foch::game::eu4::cwt::merge::suggest_for_conflict(current_file, &ast_path)
	else {
		return;
	};
	tracing::info!(
		target: "foch_merge_cwt_suggest",
		file = %current_file.display(),
		ast_path = %ast_path.join("/"),
		suggested_identity_source = ?suggestion.suggested_identity_source,
		suggested_block_policy = ?suggestion.suggested_block_policy,
		schema_provenance = %suggestion.schema_provenance,
		"cwt merge suggestion"
	);
}

impl<'a> ConflictHandler for LookupHandler<'a> {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		let metadata = ConflictMetadataView::from(view);
		if let Some(ResolutionDecision::Handler(name)) = self.matching_resolution(&metadata) {
			crate::merge::handler_registry::dispatch(name, view)
		} else {
			self.decide_metadata(&metadata)
		}
	}

	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		ConflictViewRequirement::Metadata
	}

	fn on_conflict_metadata(&mut self, view: &ConflictMetadataView) -> MetadataConflictDecision {
		if matches!(
			self.matching_resolution(view),
			Some(ResolutionDecision::Handler(_))
		) {
			MetadataConflictDecision::NeedsFullView
		} else {
			self.decide_metadata(view).into()
		}
	}

	fn set_conflict_progress(&mut self, current: usize, total: usize) {
		self.current_conflict_index = current;
		self.total_conflicts = total;
	}
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct FilesystemConfigWriter {
	path: PathBuf,
}

impl FilesystemConfigWriter {
	pub(crate) fn new(path: PathBuf) -> Self {
		Self { path }
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

	fn append_resolution(&self, entry: ResolutionEntry) -> Result<(), Box<dyn Error>> {
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

/// Single-threaded only: holds &mut self per-conflict via prompt progress,
/// deferred counters, and stdio handles. The merge engine drives this serially;
/// do NOT share across threads.
pub struct InteractiveCliHandler {
	input: Box<dyn BufRead>,
	stderr: Box<dyn Write>,
	tty_available: Option<bool>,
	current_conflict_index: usize,
	total_conflicts: usize,
	deferred_so_far: usize,
}

impl InteractiveCliHandler {
	pub fn new() -> Self {
		Self {
			input: Box::new(BufReader::new(io::stdin())),
			stderr: Box::new(io::stderr()),
			tty_available: None,
			current_conflict_index: 1,
			total_conflicts: 1,
			deferred_so_far: 0,
		}
	}

	#[cfg(test)]
	fn with_io(input: Box<dyn BufRead>, stderr: Box<dyn Write>, tty_available: bool) -> Self {
		Self {
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
			.unwrap_or_else(|| io::stdin().is_terminal() && io::stderr().is_terminal())
	}

	fn write_conflict_summary(&mut self, view: &ConflictView) {
		let address_path = view.address_path.join("/");
		let _ = writeln!(
			self.stderr,
			"[foch] unresolved structural merge conflict (conflict {}/{}) ({} deferred)",
			self.current_conflict_index, self.total_conflicts, self.deferred_so_far
		);
		let path = view.file_path.to_string_lossy();
		let _ = writeln!(self.stderr, "  file: {path}");
		let _ = writeln!(
			self.stderr,
			"  address: {address_path}/{}",
			view.address_key
		);
		let _ = writeln!(self.stderr, "  conflict_id: {}", view.conflict_id);
		let _ = writeln!(self.stderr, "  reason: {}", view.reason);
		if let Some(snippet) = &view.vanilla_snippet {
			let _ = writeln!(self.stderr, "  vanilla:");
			for line in snippet.lines().take(20) {
				let _ = writeln!(self.stderr, "      {line}");
			}
		}
		let _ = writeln!(self.stderr, "  candidates:");
		for (index, candidate) in view.candidates.iter().enumerate() {
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

	fn write_candidate_patch(&mut self, candidate: &crate::merge::conflict_view::CandidateView) {
		for summary in &candidate.change_summary {
			let _ = writeln!(self.stderr, "      {summary}");
		}
		let lines: Vec<&str> = candidate.candidate_rendered.lines().collect();
		for line in lines.iter().take(20) {
			let _ = writeln!(self.stderr, "      {line}");
		}
		let remaining = lines.len().saturating_sub(20);
		if remaining > 0 {
			let _ = writeln!(self.stderr, "      ... ({remaining} more lines)");
		}
	}

	fn write_prompt(&mut self, view: &ConflictView) {
		let mut choices = view
			.candidates
			.iter()
			.enumerate()
			.map(|(index, candidate)| format!("[{}] {}", index + 1, candidate.mod_id))
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
}

impl Default for InteractiveCliHandler {
	fn default() -> Self {
		Self::new()
	}
}

impl ConflictHandler for InteractiveCliHandler {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		if !self.stdin_stderr_are_tty() {
			let _ = writeln!(
				self.stderr,
				"[foch] interactive mode could not be entered because stdin/stderr is not a TTY; downgrading to defer"
			);
			return ConflictDecision::Defer { record: None };
		}

		self.write_conflict_summary(view);

		for attempt in 1..=3 {
			self.write_prompt(view);
			let Some(choice) = self.read_trimmed_line() else {
				return ConflictDecision::Defer { record: None };
			};
			let choice = choice.to_ascii_lowercase();
			match choice.as_str() {
				"d" | "defer" => return ConflictDecision::Defer { record: None },
				"q" | "quit" | "abort" => return ConflictDecision::Abort,
				"k" | "keep" => return ConflictDecision::KeepExisting,
				"s" | "file" | "use-file" => {
					if let Some(path) = self.prompt_for_external_path() {
						return ConflictDecision::UseFile(path);
					}
				}
				_ => {
					if let Ok(index) = choice.parse::<usize>()
						&& let Some(candidate) = index
							.checked_sub(1)
							.filter(|index| view.candidates.get(*index).is_some())
					{
						return ConflictDecision::PickCandidate {
							candidate,
							record: None,
						};
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
		ConflictDecision::Defer { record: None }
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
/// Single-threaded only: holds &mut self per-conflict while forwarding mutable
/// state into child handlers. The merge engine drives this serially; do NOT share
/// across threads.
pub struct ChainHandler<H1: ConflictHandler, H2: ConflictHandler> {
	pub first: H1,
	pub second: H2,
}

fn on_conflict_without_full_view(
	handler: &mut dyn ConflictHandler,
	view: &ConflictMetadataView,
) -> MetadataConflictDecision {
	match handler.conflict_view_requirement() {
		ConflictViewRequirement::DeferWithoutView => {
			ConflictDecision::Defer { record: None }.into()
		}
		ConflictViewRequirement::Metadata => handler.on_conflict_metadata(view),
		ConflictViewRequirement::Full => MetadataConflictDecision::NeedsFullView,
	}
}

impl<H1: ConflictHandler, H2: ConflictHandler> ConflictHandler for ChainHandler<H1, H2> {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		match self.first.on_conflict(view) {
			ConflictDecision::Defer { record: None } => self.second.on_conflict(view),
			other => other,
		}
	}

	fn conflict_view_requirement(&self) -> ConflictViewRequirement {
		if self.first.conflict_view_requirement() == ConflictViewRequirement::DeferWithoutView
			&& self.second.conflict_view_requirement() == ConflictViewRequirement::DeferWithoutView
		{
			ConflictViewRequirement::DeferWithoutView
		} else {
			ConflictViewRequirement::Metadata
		}
	}

	fn on_conflict_metadata(&mut self, view: &ConflictMetadataView) -> MetadataConflictDecision {
		match on_conflict_without_full_view(&mut self.first, view) {
			MetadataConflictDecision::Decision(ConflictDecision::Defer { record: None }) => {
				on_conflict_without_full_view(&mut self.second, view)
			}
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
	view: &ConflictView,
	current_file: &Path,
	conflict_id: &str,
	decision: &ConflictDecision,
) -> Option<ResolutionEntry> {
	match decision {
		ConflictDecision::PickCandidate { candidate, .. } => Some(ResolutionEntry {
			file: None,
			conflict_id: Some(conflict_id.to_string()),
			mod_id: None,
			r#match: None,
			prefer_mod: None,
			prefer_candidate: Some(
				view.candidates
					.get(*candidate)
					.and_then(|_| candidate.checked_add(1))?,
			),
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: None,
			policy: None,
		}),
		ConflictDecision::UseFile(path) | ConflictDecision::UseFrozenFile(path) => {
			Some(ResolutionEntry {
				file: None,
				conflict_id: Some(conflict_id.to_string()),
				mod_id: None,
				r#match: None,
				prefer_mod: None,
				prefer_candidate: None,
				use_file: Some(path.clone()),
				keep_existing: None,
				priority_boost: None,
				handler: None,
				policy: None,
			})
		}
		ConflictDecision::KeepExisting => Some(ResolutionEntry {
			file: Some(current_file.to_path_buf()),
			conflict_id: None,
			mod_id: None,
			r#match: None,
			prefer_mod: None,
			prefer_candidate: None,
			use_file: None,
			keep_existing: Some(true),
			priority_boost: None,
			handler: None,
			policy: None,
		}),
		ConflictDecision::Defer { .. } | ConflictDecision::Abort => None,
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

/// Prompts the user interactively for each surviving conflict (the post-pass
/// path: only invoked once the merge engine has finished and downstream
/// overrides have already pruned transient conflicts). Persists every Picked
/// decision to foch.toml as a side effect.
///
/// The returned outcomes carry the resolution-map decision the caller should
/// fold into the in-memory map before re-running the merge engine. If the user
/// aborts, `aborted` is set and any outcomes already collected are returned.
pub fn prompt_survivors_and_persist(
	target_path: &Path,
	survivors: &[ConflictView],
	handler: &mut dyn ConflictHandler,
	config_path: &Path,
) -> PromptSurvivorsResult {
	let total = survivors.len();
	let mut deferred_so_far = 0usize;
	let mut result = PromptSurvivorsResult::default();
	let config_writer = FilesystemConfigWriter::new(config_path.to_path_buf());
	for (idx, view) in survivors.iter().enumerate() {
		let current = idx + 1;
		let conflict_id = view.conflict_id.clone();
		handler.set_conflict_progress(current, total);
		handler.set_deferred_so_far(deferred_so_far);
		let decision = handler.on_conflict(view);
		if let Some(entry) =
			resolution_entry_for_decision(view, target_path, &view.conflict_id, &decision)
			&& let Err(err) = config_writer.append_resolution(entry)
		{
			eprintln!("[foch] failed to persist interactive resolution: {err}");
			result.aborted = true;
			break;
		}
		match decision {
			ConflictDecision::PickCandidate { candidate, .. } => {
				if view.candidates.get(candidate).is_some() {
					result.outcomes.push(PromptOutcome {
						conflict_id,
						kind: PromptOutcomeKind::Picked(ResolutionDecision::PreferCandidate(
							candidate,
						)),
					});
				} else {
					result.outcomes.push(PromptOutcome {
						conflict_id,
						kind: PromptOutcomeKind::Deferred,
					});
					deferred_so_far += 1;
				}
			}
			ConflictDecision::UseFile(path) => result.outcomes.push(PromptOutcome {
				conflict_id,
				kind: PromptOutcomeKind::Picked(ResolutionDecision::UseLiveFile(path)),
			}),
			ConflictDecision::UseFrozenFile(path) => result.outcomes.push(PromptOutcome {
				conflict_id,
				kind: PromptOutcomeKind::Picked(ResolutionDecision::UseFile(path)),
			}),
			ConflictDecision::KeepExisting => result.outcomes.push(PromptOutcome {
				conflict_id,
				kind: PromptOutcomeKind::Picked(ResolutionDecision::KeepExisting),
			}),
			ConflictDecision::Defer { .. } => {
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
	if let Some(prefer_candidate) = entry.prefer_candidate {
		table["prefer_candidate"] = value(
			i64::try_from(prefer_candidate).expect("candidate index exceeds TOML integer range"),
		);
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
	use std::cell::Cell;
	use std::collections::BTreeMap;
	use std::io::Cursor;
	use std::path::PathBuf;
	use std::rc::Rc;
	use std::time::{SystemTime, UNIX_EPOCH};

	use foch::model::ModCandidate;
	use foch::playset::PlaysetEntry;
	use foch::playset::descriptor::ModDescriptor;
	use foch::project::compute_conflict_id;

	use super::*;
	use crate::merge::conflict_view::CandidateView;

	struct TestAddress {
		path: Vec<String>,
		key: String,
	}

	struct TestConflict {
		candidates: Vec<(String, usize)>,
		reason: String,
	}

	fn address() -> TestAddress {
		TestAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "id".to_string(),
		}
	}

	fn conflict() -> TestConflict {
		TestConflict {
			candidates: Vec::new(),
			reason: "test conflict".to_string(),
		}
	}

	fn conflict_with_patches() -> TestConflict {
		conflict_with_mods(&[("mod_a", 1, "alpha"), ("mod_b", 2, "beta")])
	}

	fn conflict_with_mods(mods: &[(&str, usize, &str)]) -> TestConflict {
		TestConflict {
			candidates: mods
				.iter()
				.map(|(mod_id, precedence, _)| ((*mod_id).to_string(), *precedence))
				.collect(),
			reason: "mods disagree".to_string(),
		}
	}

	fn view_for(file: &str, address: &TestAddress, conflict: &TestConflict) -> ConflictView {
		ConflictView {
			file_path: PathBuf::from(file),
			address_path: address.path.clone(),
			address_key: address.key.clone(),
			conflict_id: compute_conflict_id(
				&PathBuf::from(file),
				&address.path.join("/"),
				&address.key,
			),
			reason: conflict.reason.clone(),
			vanilla_snippet: None,
			candidates: conflict
				.candidates
				.iter()
				.map(|(mod_id, precedence)| CandidateView {
					mod_id: mod_id.clone(),
					mod_display_name: mod_id.clone(),
					precedence: *precedence,
					change_summary: Vec::new(),
					candidate_rendered: String::new(),
				})
				.collect(),
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
			ConflictDecision::PickCandidate {
				candidate,
				record: Some(record),
			} => {
				assert_eq!(candidate, 0);
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
			entry: PlaysetEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: dependencies.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn handler_with_input(input: &str, tty_available: bool) -> InteractiveCliHandler {
		InteractiveCliHandler::with_io(
			Box::new(Cursor::new(input.as_bytes().to_vec())),
			Box::new(io::sink()),
			tty_available,
		)
	}

	#[derive(Clone, Default)]
	struct CountingHandler {
		calls: Rc<Cell<usize>>,
	}

	impl ConflictHandler for CountingHandler {
		fn on_conflict(&mut self, _: &ConflictView) -> ConflictDecision {
			self.calls.set(self.calls.get() + 1);
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			}
		}
	}

	struct RecordedDeferHandler;

	impl ConflictHandler for RecordedDeferHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			ConflictDecision::Defer {
				record: Some(HandlerResolutionRecord {
					path: view.file_path.to_string_lossy().replace('\\', "/"),
					action: "defer".to_string(),
					source: None,
					rationale: Some("matched DSL handler=defer rule".to_string()),
				}),
			}
		}
	}

	#[test]
	fn lookup_handler_replays_generic_patch_address_conflict_id() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = BTreeMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod_a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = LookupHandler::new(&map, current_file);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(
			decision,
			ConflictDecision::PickCandidate {
				candidate: 0,
				record: None
			}
		);
	}

	#[test]
	fn lookup_handler_prefers_exact_view_then_target_address_then_file_and_pattern() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let address = address();
		let conflict = conflict_with_patches();
		let address_conflict_id =
			compute_conflict_id(&current_file, &address.path.join("/"), &address.key);
		let mut view = view_for("events/PirateEvents.txt", &address, &conflict);
		view.conflict_id = "a".repeat(64);

		let exact_map = ResolutionMap {
			by_conflict_id: BTreeMap::from([
				(
					view.conflict_id.clone(),
					ResolutionDecision::PreferMod("mod_b".to_string()),
				),
				(
					address_conflict_id.clone(),
					ResolutionDecision::PreferMod("mod_a".to_string()),
				),
			]),
			by_file: BTreeMap::from([(
				current_file.clone(),
				ResolutionDecision::PreferMod("mod_a".to_string()),
			)]),
			..ResolutionMap::default()
		};
		assert_eq!(
			LookupHandler::new(&exact_map, current_file.clone()).on_conflict(&view),
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			},
			"the exact public view id must beat the target address id and file rule",
		);

		let address_only_map = ResolutionMap {
			by_conflict_id: BTreeMap::from([(
				address_conflict_id.clone(),
				ResolutionDecision::PreferMod("mod_a".to_string()),
			)]),
			..ResolutionMap::default()
		};
		assert_eq!(
			LookupHandler::new(&address_only_map, current_file.clone()).on_conflict(&view),
			ConflictDecision::Defer { record: None },
			"an address-derived id must not select a semantic candidate sequence",
		);

		let address_view = view_for("root/event/id", &address, &conflict);
		let address_map = ResolutionMap {
			by_conflict_id: BTreeMap::from([(
				address_conflict_id,
				ResolutionDecision::PreferMod("mod_a".to_string()),
			)]),
			by_file: BTreeMap::from([(
				current_file.clone(),
				ResolutionDecision::PreferMod("mod_b".to_string()),
			)]),
			..ResolutionMap::default()
		};
		assert_eq!(
			LookupHandler::new(&address_map, current_file.clone()).on_conflict(&address_view),
			ConflictDecision::PickCandidate {
				candidate: 0,
				record: None,
			},
			"the target address fallback must run before the file rule",
		);

		let (file_matcher, leaf_matcher) =
			foch::project::parse_match_dsl("events/**").expect("compile test pattern");
		let file_map = ResolutionMap {
			by_file: BTreeMap::from([(
				current_file.clone(),
				ResolutionDecision::PreferMod("mod_b".to_string()),
			)]),
			pattern_rules: vec![foch::project::PatternRule {
				dsl: "events/**".to_string(),
				file_matcher,
				leaf_matcher,
				decision: ResolutionDecision::PreferMod("mod_a".to_string()),
			}],
			..ResolutionMap::default()
		};
		assert_eq!(
			LookupHandler::new(&file_map, current_file).on_conflict(&view),
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			},
			"the legacy miss must preserve file-over-pattern precedence",
		);
	}

	#[test]
	fn lookup_handler_returns_defer_on_miss() {
		let map = ResolutionMap::default();
		let mut handler = LookupHandler::new(&map, PathBuf::from("events/PirateEvents.txt"));

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn lookup_handler_requests_full_view_only_after_a_named_handler_matches() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let map = ResolutionMap {
			by_file: BTreeMap::from([(
				current_file.clone(),
				ResolutionDecision::Handler("defer".to_string()),
			)]),
			..ResolutionMap::default()
		};
		let mut handler = LookupHandler::new(&map, current_file);

		assert_eq!(
			handler.conflict_view_requirement(),
			ConflictViewRequirement::Metadata,
		);
		let view = view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		);
		assert_eq!(
			handler.on_conflict_metadata(&ConflictMetadataView::from(&view)),
			MetadataConflictDecision::NeedsFullView,
		);
	}

	#[test]
	fn lookup_handler_defers_when_prefer_mod_is_not_an_exact_candidate() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let map = ResolutionMap {
			by_conflict_id: BTreeMap::from([(
				conflict_id,
				ResolutionDecision::PreferMod("same_mod".to_string()),
			)]),
			..ResolutionMap::default()
		};
		let conflict = conflict_with_mods(&[("same_mod", 1, "alpha"), ("same_mod", 2, "beta")]);
		let mut handler = LookupHandler::new(&map, current_file);

		let decision =
			handler.on_conflict(&view_for("events/PirateEvents.txt", &address(), &conflict));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn lookup_handler_replays_exact_candidate_when_mod_ids_repeat() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let map = ResolutionMap {
			by_conflict_id: BTreeMap::from([(conflict_id, ResolutionDecision::PreferCandidate(1))]),
			..ResolutionMap::default()
		};
		let conflict = conflict_with_mods(&[("same_mod", 1, "alpha"), ("same_mod", 2, "beta")]);
		let mut handler = LookupHandler::new(&map, current_file);

		let decision =
			handler.on_conflict(&view_for("events/PirateEvents.txt", &address(), &conflict));

		assert_eq!(
			decision,
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			}
		);
	}

	#[test]
	fn lookup_handler_chained_with_defer_uses_resolution_then_defers() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = BTreeMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod_a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, current_file),
			second: DeferHandler,
		};
		let miss = TestAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "other".to_string(),
		};
		assert_eq!(
			handler.conflict_view_requirement(),
			ConflictViewRequirement::Metadata,
		);

		let resolved_view = view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		);
		let deferred_view = view_for("events/PirateEvents.txt", &miss, &conflict());
		let resolved = handler.on_conflict_metadata(&ConflictMetadataView::from(&resolved_view));
		let deferred = handler.on_conflict_metadata(&ConflictMetadataView::from(&deferred_view));

		assert_eq!(
			resolved,
			MetadataConflictDecision::Decision(ConflictDecision::PickCandidate {
				candidate: 0,
				record: None
			})
		);
		assert_eq!(
			deferred,
			MetadataConflictDecision::Decision(ConflictDecision::Defer { record: None })
		);
	}

	#[test]
	fn chain_handler_does_not_fall_through_recorded_defer() {
		let calls = Rc::new(Cell::new(0));
		let mut handler = ChainHandler {
			first: RecordedDeferHandler,
			second: CountingHandler {
				calls: Rc::clone(&calls),
			},
		};

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict(),
		));

		match decision {
			ConflictDecision::Defer {
				record: Some(record),
			} => assert_eq!(record.action, "defer"),
			other => panic!("expected recorded defer, got {other:?}"),
		}
		assert_eq!(calls.get(), 0);
	}

	#[test]
	fn dep_implies_resolution_picks_two_mod_downstream() {
		let mut handler = dep_handler(&[("mod_a", "mod_b")]);

		let decision = handler.on_conflict(&view_for(
			"common/ideas/dep.txt",
			&address(),
			&conflict_with_patches(),
		));

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

		let decision =
			handler.on_conflict(&view_for("common/ideas/dep.txt", &address(), &conflict));

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

		let decision =
			handler.on_conflict(&view_for("common/ideas/dep.txt", &address(), &conflict));

		assert_dep_pick(decision, "mod_a", "mod mod_a declares dep on mod_b");
	}

	#[test]
	fn dep_implies_resolution_defers_independent_mods() {
		let mut handler = dep_handler(&[]);

		let decision = handler.on_conflict(&view_for(
			"common/ideas/dep.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn dep_implies_resolution_defers_when_any_contributor_is_independent() {
		let mut handler = dep_handler(&[("mod_a", "mod_b")]);
		let conflict = conflict_with_mods(&[
			("mod_a", 3, "alpha"),
			("mod_b", 1, "beta"),
			("mod_c", 2, "gamma"),
		]);

		let decision =
			handler.on_conflict(&view_for("common/ideas/dep.txt", &address(), &conflict));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn dep_implies_resolution_defers_on_cycle() {
		let mut handler = dep_handler(&[("mod_a", "mod_b"), ("mod_b", "mod_a")]);

		let decision = handler.on_conflict(&view_for(
			"common/ideas/dep.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
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
			&[foch::project::DepOverride::new("mod_a", "mod_b")],
		);
		let mut handler =
			DepImpliesResolutionHandler::new(PathBuf::from("common/ideas/dep.txt"), graph);

		let decision = handler.on_conflict(&view_for(
			"common/ideas/dep.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn interactive_handler_returns_defer_on_non_tty() {
		let mut handler = handler_with_input("1\n", false);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn interactive_handler_returns_exact_candidate_on_user_choice() {
		let mut handler = handler_with_input("2\n", true);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(
			decision,
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None
			}
		);
	}

	#[test]
	fn prompt_survivors_persists_resolution_to_config_writer() {
		let root = project_test_dir("prompt_survivors_persists_resolution_to_config_writer");
		let config_path = root.join("foch.toml");
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let mut handler = handler_with_input("1\n", true);
		let survivor_address = address();
		let survivor_conflict = conflict_with_patches();
		let survivors = vec![view_for(
			"events/PirateEvents.txt",
			&survivor_address,
			&survivor_conflict,
		)];

		let result =
			prompt_survivors_and_persist(&current_file, &survivors, &mut handler, &config_path);

		assert!(!result.aborted);
		assert_eq!(result.outcomes.len(), 1);
		let content = fs::read_to_string(&config_path).expect("read config");
		assert!(content.contains("[[resolutions]]"));
		assert!(content.contains("prefer_candidate = 1"));
		assert!(content.contains(&compute_conflict_id(&current_file, "root/event", "id")));
	}

	#[test]
	fn interactive_handler_returns_keep_existing_on_user_choice_k() {
		let mut handler = handler_with_input("k\n", true);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::KeepExisting);
	}

	#[test]
	fn interactive_handler_invalid_input_eventually_defers() {
		let mut handler = handler_with_input("x\ny\n0\n", true);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(decision, ConflictDecision::Defer { record: None });
	}

	#[test]
	fn interactive_handler_returns_use_file_resolution() {
		let mut handler = handler_with_input("s\nresolutions/PirateEvents.txt\n", true);

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(
			decision,
			ConflictDecision::UseFile(PathBuf::from("resolutions/PirateEvents.txt"))
		);
	}

	#[test]
	fn merge_command_with_interactive_handler_chains_handlers_correctly() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = BTreeMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod_a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let calls = Rc::new(Cell::new(0));
		let interactive = CountingHandler {
			calls: Rc::clone(&calls),
		};
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, current_file),
			second: ChainHandler {
				first: interactive,
				second: DeferHandler,
			},
		};

		let decision = handler.on_conflict(&view_for(
			"events/PirateEvents.txt",
			&address(),
			&conflict_with_patches(),
		));

		assert_eq!(
			decision,
			ConflictDecision::PickCandidate {
				candidate: 0,
				record: None
			}
		);
		assert_eq!(
			calls.get(),
			0,
			"lookup hit should not invoke interactive handler"
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
		let writer = FilesystemConfigWriter::new(path.clone());

		writer
			.append_resolution(ResolutionEntry {
				file: None,
				conflict_id: Some("abc12345".to_string()),
				mod_id: None,
				r#match: None,
				prefer_mod: Some("mod_a".to_string()),
				prefer_candidate: None,
				use_file: None,
				keep_existing: None,
				priority_boost: None,
				handler: None,
				policy: None,
			})
			.expect("append resolution");

		let content = fs::read_to_string(&path).expect("read config");
		assert!(content.contains("# keep this comment"));
		assert!(content.contains("[[overrides]]"));
		assert!(content.contains("[[resolutions]]"));
		assert!(content.contains(r#"conflict_id = "abc12345""#));
		assert!(content.contains(r#"prefer_mod = "mod_a""#));
		let parsed = foch::project::Project::from_toml_str(&content).expect("parse config");
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
