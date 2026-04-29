//! Per-(mod, file) dependency-graph base resolver.
//!
//! DAG-driven N-way merge base resolution (see
//! `docs/dag-merge-design.md`). The merge pipeline uses [`BaseResolver`] to
//! replace linear chained-diff ancestry with per-(mod, file) recursive bases.
//!
//! Scope of this file:
//! * Build a mod-level dependency DAG from declared `descriptor.mod`
//!   dependencies, resolved against [`ModIdentityIndex`].
//! * Detect cycles via Tarjan's SCC and break them deterministically by
//!   playlist position.
//! * Restrict the global DAG to the subset of mods shipping a particular
//!   file (`induced_file_dag`), lifting edges through skipped nodes.
//! * Apply `replace_path` semantics: drop earlier contributors under the
//!   replaced prefix and force the replacing mod's per-file base to
//!   [`BaseSourceKind::Empty`].
//! * Provide [`BaseResolver`] with `(parent_set, file_path)` memoization and
//!   recursive merged-base synthesis through the existing `merge_patch_sets` +
//!   `apply_patches` pipeline.

#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use foch_core::domain::dep_resolution::ModIdentityIndex;
use foch_core::model::ModCandidate;
use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies, ScriptFileKind};
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

use super::patch::{diff_ast, fold_renames};
use super::patch_apply::apply_patches;
use super::patch_merge::{PatchResolution, merge_patch_sets};
use crate::workspace::ResolvedFileContributor;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Stable identifier for a mod within a single DAG. Currently the same
/// `mod_id` string `ModCandidate` exposes (typically the steam id, or a
/// synthesized id for local mods). Wrapping it as a newtype keeps the API
/// honest: a `ModId` is never confused with an arbitrary `String`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ModId(pub String);

impl ModId {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl From<&str> for ModId {
	fn from(s: &str) -> Self {
		Self(s.to_string())
	}
}

impl From<String> for ModId {
	fn from(s: String) -> Self {
		Self(s)
	}
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DagDiagnosticSeverity {
	Warning,
	Info,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DagDiagnosticKind {
	/// `mod_id` declared a dep string that does not resolve in the playset.
	MissingDependency { mod_id: ModId, dep_token: String },
	/// A cycle was detected; lists the mods involved (in topo input order).
	DependencyCycle { members: Vec<ModId> },
	/// An edge was removed to break a cycle.
	BrokenCycleEdge { child: ModId, parent: ModId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DagDiagnostic {
	pub severity: DagDiagnosticSeverity,
	pub kind: DagDiagnosticKind,
}

// ---------------------------------------------------------------------------
// ModDag (mod-level)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct ModDag {
	/// Child → parents (in declaration order, deduplicated).
	parents: HashMap<ModId, Vec<ModId>>,
	/// Parent → children (deterministic order = playlist position of the
	/// child).
	children: HashMap<ModId, Vec<ModId>>,
	/// Topological order (parents before children).
	topo: Vec<ModId>,
	/// Playlist position for each mod (stable break tiebreak).
	position: HashMap<ModId, usize>,
	/// Mods declared a dep that wasn't in the playset (collected for
	/// diagnostics; the dep is treated as absent for DAG purposes).
	missing_deps: Vec<(ModId, String)>,
	/// `replace_path` prefixes per mod (already trimmed of leading/trailing
	/// slashes for direct prefix-matching).
	replace_paths: HashMap<ModId, Vec<String>>,
}

impl ModDag {
	pub fn parents_of(&self, mod_id: &ModId) -> &[ModId] {
		self.parents
			.get(mod_id)
			.map(|v| v.as_slice())
			.unwrap_or(&[])
	}

	pub fn children_of(&self, mod_id: &ModId) -> &[ModId] {
		self.children
			.get(mod_id)
			.map(|v| v.as_slice())
			.unwrap_or(&[])
	}

	pub fn topo(&self) -> &[ModId] {
		&self.topo
	}

	pub fn missing_deps(&self) -> &[(ModId, String)] {
		&self.missing_deps
	}

	pub fn position(&self, mod_id: &ModId) -> Option<usize> {
		self.position.get(mod_id).copied()
	}

	pub fn replace_paths(&self, mod_id: &ModId) -> &[String] {
		self.replace_paths
			.get(mod_id)
			.map(|v| v.as_slice())
			.unwrap_or(&[])
	}
}

// ---------------------------------------------------------------------------
// build_mod_dag
// ---------------------------------------------------------------------------

/// Build the mod-level dependency DAG from `mods` (sorted by playlist
/// position). Returns the DAG plus diagnostics for missing deps and cycles.
pub fn build_mod_dag(mods: &[ModCandidate]) -> (ModDag, Vec<DagDiagnostic>) {
	let identity = ModIdentityIndex::from_mods(mods);
	let mut diagnostics = Vec::new();

	let ids: Vec<ModId> = mods.iter().map(|m| ModId(m.mod_id.clone())).collect();
	let mut position: HashMap<ModId, usize> = HashMap::new();
	for (idx, id) in ids.iter().enumerate() {
		position.insert(id.clone(), idx);
	}

	let mut parents: HashMap<ModId, Vec<ModId>> = HashMap::new();
	let mut missing_deps: Vec<(ModId, String)> = Vec::new();
	let mut replace_paths: HashMap<ModId, Vec<String>> = HashMap::new();

	for (idx, candidate) in mods.iter().enumerate() {
		let me = ids[idx].clone();
		let descriptor = match candidate.descriptor.as_ref() {
			Some(d) => d,
			None => {
				parents.entry(me.clone()).or_default();
				continue;
			}
		};

		// Resolve declared deps.
		let mut my_parents: Vec<ModId> = Vec::new();
		let mut seen: HashSet<ModId> = HashSet::new();
		for dep_token in &descriptor.dependencies {
			match identity.lookup(dep_token) {
				Some(parent_idx) if parent_idx != idx => {
					let parent_id = ids[parent_idx].clone();
					if seen.insert(parent_id.clone()) {
						my_parents.push(parent_id);
					}
				}
				Some(_) => {
					// Self-dep: silently drop (not actionable).
				}
				None => {
					diagnostics.push(DagDiagnostic {
						severity: DagDiagnosticSeverity::Warning,
						kind: DagDiagnosticKind::MissingDependency {
							mod_id: me.clone(),
							dep_token: dep_token.clone(),
						},
					});
					missing_deps.push((me.clone(), dep_token.clone()));
				}
			}
		}
		parents.insert(me.clone(), my_parents);

		// Record replace_path prefixes (normalized).
		if !descriptor.replace_path.is_empty() {
			let cleaned: Vec<String> = descriptor
				.replace_path
				.iter()
				.map(|p| normalize_path_prefix(p))
				.filter(|p| !p.is_empty())
				.collect();
			if !cleaned.is_empty() {
				replace_paths.insert(me.clone(), cleaned);
			}
		}
	}

	// Cycle detection + deterministic break.
	break_cycles(&ids, &mut parents, &position, &mut diagnostics);

	// Build children (deterministic by playlist position).
	let mut children: HashMap<ModId, Vec<ModId>> = HashMap::new();
	for child in &ids {
		for parent in parents.get(child).cloned().unwrap_or_default() {
			children.entry(parent).or_default().push(child.clone());
		}
	}
	for v in children.values_mut() {
		v.sort_by_key(|c| position.get(c).copied().unwrap_or(usize::MAX));
		v.dedup();
	}

	// Topological sort using Kahn's algorithm with deterministic ordering by
	// playlist position.
	let topo = topo_sort(&ids, &parents);

	let dag = ModDag {
		parents,
		children,
		topo,
		position,
		missing_deps,
		replace_paths,
	};
	(dag, diagnostics)
}

fn normalize_path_prefix(raw: &str) -> String {
	raw.trim().trim_matches('/').replace('\\', "/").to_string()
}

/// Tarjan SCC: find any cycle, then drop the edge whose *child* has the
/// highest playlist position (loaded latest). Repeat until acyclic.
fn break_cycles(
	ids: &[ModId],
	parents: &mut HashMap<ModId, Vec<ModId>>,
	position: &HashMap<ModId, usize>,
	diagnostics: &mut Vec<DagDiagnostic>,
) {
	loop {
		let sccs = tarjan_sccs(ids, parents);
		// A cycle is any SCC of size ≥ 2, or a self-loop (size 1 with edge
		// to itself — already filtered in build_mod_dag, but defensive).
		let cyclic_scc = sccs.into_iter().find(|s| {
			s.len() > 1
				|| s.iter()
					.any(|m| parents.get(m).is_some_and(|ps| ps.contains(m)))
		});
		let scc = match cyclic_scc {
			Some(s) => s,
			None => return,
		};

		diagnostics.push(DagDiagnostic {
			severity: DagDiagnosticSeverity::Warning,
			kind: DagDiagnosticKind::DependencyCycle {
				members: scc.clone(),
			},
		});

		// Find the child with the highest position whose parents contain a
		// member of the SCC. Drop that edge (child → parent).
		let scc_set: HashSet<&ModId> = scc.iter().collect();
		let mut victim: Option<(ModId, ModId)> = None;
		let mut victim_pos: usize = 0;
		for child in &scc {
			if let Some(ps) = parents.get(child) {
				for p in ps {
					if scc_set.contains(p) {
						let pos = position.get(child).copied().unwrap_or(0);
						if victim.is_none() || pos > victim_pos {
							victim = Some((child.clone(), p.clone()));
							victim_pos = pos;
						}
					}
				}
			}
		}

		let (child, parent) = match victim {
			Some(v) => v,
			None => return, // shouldn't happen
		};

		if let Some(ps) = parents.get_mut(&child) {
			ps.retain(|p| p != &parent);
		}
		diagnostics.push(DagDiagnostic {
			severity: DagDiagnosticSeverity::Info,
			kind: DagDiagnosticKind::BrokenCycleEdge { child, parent },
		});
	}
}

/// Tarjan's SCC algorithm. Iteration order over `ids` is deterministic
/// (= playlist position).
fn tarjan_sccs(ids: &[ModId], parents: &HashMap<ModId, Vec<ModId>>) -> Vec<Vec<ModId>> {
	struct State<'a> {
		index_counter: usize,
		stack: Vec<&'a ModId>,
		on_stack: HashSet<&'a ModId>,
		index: HashMap<&'a ModId, usize>,
		lowlink: HashMap<&'a ModId, usize>,
		sccs: Vec<Vec<ModId>>,
	}

	fn strongconnect<'a>(
		v: &'a ModId,
		state: &mut State<'a>,
		parents: &'a HashMap<ModId, Vec<ModId>>,
	) {
		state.index.insert(v, state.index_counter);
		state.lowlink.insert(v, state.index_counter);
		state.index_counter += 1;
		state.stack.push(v);
		state.on_stack.insert(v);

		if let Some(ps) = parents.get(v) {
			for w in ps {
				if !state.index.contains_key(w) {
					strongconnect(w, state, parents);
					let wl = state.lowlink[w];
					let vl = state.lowlink[v];
					state.lowlink.insert(v, vl.min(wl));
				} else if state.on_stack.contains(w) {
					let widx = state.index[w];
					let vl = state.lowlink[v];
					state.lowlink.insert(v, vl.min(widx));
				}
			}
		}

		if state.lowlink[v] == state.index[v] {
			let mut scc: Vec<ModId> = Vec::new();
			while let Some(w) = state.stack.pop() {
				state.on_stack.remove(w);
				scc.push(w.clone());
				if w == v {
					break;
				}
			}
			state.sccs.push(scc);
		}
	}

	let mut state = State {
		index_counter: 0,
		stack: Vec::new(),
		on_stack: HashSet::new(),
		index: HashMap::new(),
		lowlink: HashMap::new(),
		sccs: Vec::new(),
	};

	for v in ids {
		if !state.index.contains_key(v) {
			strongconnect(v, &mut state, parents);
		}
	}
	state.sccs
}

/// Kahn's algorithm with stable ordering = playlist position.
fn topo_sort(ids: &[ModId], parents: &HashMap<ModId, Vec<ModId>>) -> Vec<ModId> {
	// in-degree = number of parents. Nodes with zero parents go first.
	let mut indeg: HashMap<&ModId, usize> = HashMap::new();
	for id in ids {
		indeg.insert(id, parents.get(id).map(|p| p.len()).unwrap_or(0));
	}
	// children index for the iteration.
	let mut child_index: HashMap<&ModId, Vec<&ModId>> = HashMap::new();
	for id in ids {
		if let Some(ps) = parents.get(id) {
			for p in ps {
				child_index.entry(p).or_default().push(id);
			}
		}
	}

	let mut ready: Vec<&ModId> = ids.iter().filter(|id| indeg[id] == 0).collect();
	let mut out: Vec<ModId> = Vec::with_capacity(ids.len());
	while !ready.is_empty() {
		// Stable: pop the lowest playlist position.
		ready.sort_by_key(|id| ids.iter().position(|x| x == *id).unwrap_or(usize::MAX));
		let next = ready.remove(0);
		out.push(next.clone());
		if let Some(cs) = child_index.get(next) {
			for c in cs {
				let e = indeg.get_mut(c).expect("child indeg present");
				*e -= 1;
				if *e == 0 {
					ready.push(*c);
				}
			}
		}
	}
	// If the graph still has cycles (shouldn't after break_cycles), append
	// the leftover in playlist order so callers still get every node.
	if out.len() < ids.len() {
		let placed: HashSet<ModId> = out.iter().cloned().collect();
		for id in ids {
			if !placed.contains(id) {
				out.push(id.clone());
			}
		}
	}
	out
}

// ---------------------------------------------------------------------------
// FileDag (per-file induced subgraph)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct FileDag {
	pub file_path: String,
	/// Contributing mods in playlist order (after replace_path filtering).
	contributors: Vec<ModId>,
	contributor_set: HashSet<ModId>,
	/// Per-mod parents (lifted ancestor closure restricted to contributors).
	parents: HashMap<ModId, Vec<ModId>>,
	/// Mods whose per-file base is forced empty by their own replace_path.
	replace_path_owners: HashSet<ModId>,
	position: HashMap<ModId, usize>,
}

impl FileDag {
	pub fn file_path(&self) -> &str {
		&self.file_path
	}
	pub fn contributors(&self) -> &[ModId] {
		&self.contributors
	}
	pub fn parents_of(&self, mod_id: &ModId) -> &[ModId] {
		self.parents
			.get(mod_id)
			.map(|v| v.as_slice())
			.unwrap_or(&[])
	}
	pub fn ancestors_of(&self, mod_id: &ModId) -> Vec<ModId> {
		let mut out = Vec::new();
		let mut seen = HashSet::new();
		self.collect_ancestors(mod_id, &mut seen, &mut out);
		out.sort_by_key(|m| self.precedence_of(m));
		out.dedup();
		out
	}
	fn collect_ancestors(&self, mod_id: &ModId, seen: &mut HashSet<ModId>, out: &mut Vec<ModId>) {
		for parent in self.parents_of(mod_id) {
			if seen.insert(parent.clone()) {
				self.collect_ancestors(parent, seen, out);
				out.push(parent.clone());
			}
		}
	}
	pub fn precedence_of(&self, mod_id: &ModId) -> usize {
		self.position.get(mod_id).copied().unwrap_or(usize::MAX)
	}
	pub fn ships(&self, mod_id: &ModId) -> bool {
		self.contributor_set.contains(mod_id)
	}
	pub fn replaces_path(&self, mod_id: &ModId) -> bool {
		self.replace_path_owners.contains(mod_id)
	}
}

/// Configuration for `replace_path` semantics. P3 will plumb this through
/// the merge CLI as `--ignore-replace-path[=mod_id|=all]`.
#[derive(Clone, Debug, Default)]
pub enum IgnoreReplacePath {
	#[default]
	None,
	Mods(HashSet<ModId>),
	All,
}

impl IgnoreReplacePath {
	fn applies_to(&self, mod_id: &ModId) -> bool {
		match self {
			IgnoreReplacePath::None => false,
			IgnoreReplacePath::All => true,
			IgnoreReplacePath::Mods(ms) => ms.contains(mod_id),
		}
	}
}

/// Build the per-file induced subgraph. `contributors` is the file
/// inventory entry (already sorted by precedence; base-game and synthetic
/// base entries are filtered out by the caller — they are *not* part of
/// the DAG node set).
pub fn induced_file_dag(
	global: &ModDag,
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	ignore: &IgnoreReplacePath,
) -> FileDag {
	let normalized_file = normalize_path_prefix(file_path);

	// Initial contributor list (mod ids only — base-game/synthetic dropped).
	let mut active: Vec<(ModId, usize)> = contributors
		.iter()
		.filter(|c| !c.is_base_game && !c.is_synthetic_base)
		.map(|c| (ModId(c.mod_id.clone()), c.precedence))
		.collect();
	active.sort_by_key(|(_, p)| *p);

	// Apply replace_path filtering: for any mod M in `active` that owns a
	// replace_path covering file_path, drop earlier-precedence contributors.
	// Higher-precedence contributors stay (loader semantics).
	let mut replace_path_owners: HashSet<ModId> = HashSet::new();
	let mut drop_before: Option<usize> = None;
	for (mid, prec) in &active {
		if ignore.applies_to(mid) {
			continue;
		}
		let prefixes = global.replace_paths(mid);
		if prefixes.is_empty() {
			continue;
		}
		let covers = prefixes
			.iter()
			.any(|p| normalized_file == *p || normalized_file.starts_with(&format!("{p}/")));
		if covers {
			replace_path_owners.insert(mid.clone());
			// Drop everything with strictly lower precedence than `prec`
			// from active. Track the highest such cut-off.
			drop_before = Some(drop_before.map_or(*prec, |c| c.max(*prec)));
		}
	}
	if let Some(cutoff) = drop_before {
		active.retain(|(mid, prec)| *prec >= cutoff || replace_path_owners.contains(mid));
	}

	let contributor_set: HashSet<ModId> = active.iter().map(|(m, _)| m.clone()).collect();
	let contributors_ordered: Vec<ModId> = active.iter().map(|(m, _)| m.clone()).collect();
	let mut position: HashMap<ModId, usize> = HashMap::new();
	for (i, m) in contributors_ordered.iter().enumerate() {
		position.insert(m.clone(), i);
	}

	// Lift edges through skipped (non-contributing) ancestors.
	let mut parents: HashMap<ModId, Vec<ModId>> = HashMap::new();
	for child in &contributors_ordered {
		if replace_path_owners.contains(child) {
			// Replace_path means "fresh start" — no parents.
			parents.insert(child.clone(), Vec::new());
			continue;
		}
		let lifted = lift_ancestor_edges(global, child, &contributor_set);
		parents.insert(child.clone(), lifted);
	}

	FileDag {
		file_path: file_path.to_string(),
		contributors: contributors_ordered,
		contributor_set,
		parents,
		replace_path_owners,
		position,
	}
}

/// Walk up `child`'s ancestors in the global DAG. For each ancestor that is
/// also a contributor, record it as a (lifted) parent and stop. For
/// non-contributing ancestors, recurse through *their* parents — this
/// implements the transitive ancestor closure restricted to `members`.
fn lift_ancestor_edges(global: &ModDag, child: &ModId, members: &HashSet<ModId>) -> Vec<ModId> {
	let mut out: Vec<ModId> = Vec::new();
	let mut seen: HashSet<ModId> = HashSet::new();
	let mut stack: Vec<ModId> = global.parents_of(child).to_vec();
	while let Some(p) = stack.pop() {
		if !seen.insert(p.clone()) {
			continue;
		}
		if members.contains(&p) {
			out.push(p);
		} else {
			for pp in global.parents_of(&p) {
				stack.push(pp.clone());
			}
		}
	}
	// Deterministic ordering: by topo position in `global`.
	let topo_pos: HashMap<&ModId, usize> = global
		.topo()
		.iter()
		.enumerate()
		.map(|(i, m)| (m, i))
		.collect();
	out.sort_by_key(|m| topo_pos.get(m).copied().unwrap_or(usize::MAX));
	out.dedup();
	out
}

// ---------------------------------------------------------------------------
// BaseResolver
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaseSourceKind {
	/// Diff against the vanilla base game (or `None` if no vanilla version
	/// exists for this file — same as today's `diff_ast_as_inserts` path).
	Vanilla,
	/// Diff against an empty file (replace_path semantics).
	Empty,
	/// Diff against a synthesized merge of the listed parents.
	Synthesized,
}

#[derive(Clone, Debug)]
pub struct ResolvedBase {
	pub kind: BaseSourceKind,
	/// For [`BaseSourceKind::Synthesized`], the transitive contributor set whose
	/// merged AST serves as the diff base. Empty for `Vanilla` and `Empty`.
	pub parents: BTreeSet<ModId>,
}

/// Concrete payload for a resolved base.
#[derive(Clone, Debug)]
pub enum BaseSource {
	Vanilla,
	Empty,
	Synthesized(Rc<ParsedScriptFile>),
}

/// Memoizing resolver. One instance is intended to live for the duration
/// of a single merge run.
pub struct BaseResolver {
	cache_subset: HashMap<(BTreeSet<ModId>, String), Option<Rc<ParsedScriptFile>>>,
	ignore_replace_path: IgnoreReplacePath,
}

impl BaseResolver {
	pub fn new(ignore_replace_path: IgnoreReplacePath) -> Self {
		Self {
			cache_subset: HashMap::new(),
			ignore_replace_path,
		}
	}

	pub fn ignore_replace_path(&self) -> &IgnoreReplacePath {
		&self.ignore_replace_path
	}

	/// Compute the base classification for `mod_id`'s view of the file
	/// described by `file_dag`. This does not synthesize any AST — it
	/// identifies the kind of base and the transitive parent set that recursive
	/// merge will need to consume.
	pub fn resolve_base(&self, file_dag: &FileDag, mod_id: &ModId) -> ResolvedBase {
		if file_dag.replaces_path(mod_id) {
			return ResolvedBase {
				kind: BaseSourceKind::Empty,
				parents: BTreeSet::new(),
			};
		}
		let parents: BTreeSet<ModId> = file_dag.ancestors_of(mod_id).into_iter().collect();
		if parents.is_empty() {
			ResolvedBase {
				kind: BaseSourceKind::Vanilla,
				parents,
			}
		} else {
			ResolvedBase {
				kind: BaseSourceKind::Synthesized,
				parents,
			}
		}
	}

	/// Memoized recursive merge of `parents` for `file_dag.file_path()`.
	pub fn compute_merged_base(
		&mut self,
		parents: &BTreeSet<ModId>,
		file_dag: &FileDag,
		vanilla: Option<&ParsedScriptFile>,
		contributors: &HashMap<ModId, ParsedScriptFile>,
		merge_key_source: MergeKeySource,
		policies: &MergePolicies,
	) -> Option<Rc<ParsedScriptFile>> {
		let file_path = file_dag.file_path();
		self.merged_base_or_compute(parents, file_path, |resolver| {
			resolver.compute_merged_base_uncached(
				parents,
				file_dag,
				vanilla,
				contributors,
				merge_key_source,
				policies,
			)
		})
	}

	fn compute_merged_base_uncached(
		&mut self,
		parents: &BTreeSet<ModId>,
		file_dag: &FileDag,
		vanilla: Option<&ParsedScriptFile>,
		contributors: &HashMap<ModId, ParsedScriptFile>,
		merge_key_source: MergeKeySource,
		policies: &MergePolicies,
	) -> Option<Rc<ParsedScriptFile>> {
		let mut mod_patches = Vec::new();
		let mut ordered: Vec<ModId> = parents.iter().cloned().collect();
		ordered.sort_by_key(|m| file_dag.precedence_of(m));

		for parent in ordered {
			let Some(parent_ast) = contributors.get(&parent) else {
				continue;
			};
			let parent_base = self.resolve_base(file_dag, &parent);
			let base_ast = self.base_source_ast(
				&parent_base,
				file_dag,
				vanilla,
				contributors,
				merge_key_source,
				policies,
			)?;
			let patches = fold_renames(diff_ast(&base_ast, parent_ast, merge_key_source));
			mod_patches.push((parent.0.clone(), file_dag.precedence_of(&parent), patches));
		}

		let merge_result = merge_patch_sets(mod_patches, policies);
		if !merge_result.conflicts.is_empty() {
			return None;
		}
		let resolved_patches = resolved_patches(&merge_result.resolved);
		let base_statements = if parents.iter().any(|p| file_dag.replaces_path(p)) {
			Vec::new()
		} else {
			vanilla
				.map(|base| base.ast.statements.clone())
				.unwrap_or_default()
		};
		let merged_statements =
			apply_patches(&base_statements, &resolved_patches, merge_key_source);
		let template = template_for(file_dag, vanilla, contributors);
		Some(Rc::new(synthesized_parsed_file(
			file_dag.file_path(),
			template,
			merged_statements,
		)))
	}

	fn base_source_ast(
		&mut self,
		resolved: &ResolvedBase,
		file_dag: &FileDag,
		vanilla: Option<&ParsedScriptFile>,
		contributors: &HashMap<ModId, ParsedScriptFile>,
		merge_key_source: MergeKeySource,
		policies: &MergePolicies,
	) -> Option<Rc<ParsedScriptFile>> {
		match resolved.kind {
			BaseSourceKind::Vanilla => Some(Rc::new(match vanilla {
				Some(base) => base.clone(),
				None => synthesized_parsed_file(
					file_dag.file_path(),
					template_for(file_dag, vanilla, contributors),
					Vec::new(),
				),
			})),
			BaseSourceKind::Empty => Some(Rc::new(synthesized_parsed_file(
				file_dag.file_path(),
				template_for(file_dag, vanilla, contributors),
				Vec::new(),
			))),
			BaseSourceKind::Synthesized => self.compute_merged_base(
				&resolved.parents,
				file_dag,
				vanilla,
				contributors,
				merge_key_source,
				policies,
			),
		}
	}

	/// Cache primitive: returns the cached value for `(parents, file_path)`
	/// if present, otherwise calls `f`, caches the result, and returns it.
	pub fn merged_base_or_compute<F>(
		&mut self,
		parents: &BTreeSet<ModId>,
		file_path: &str,
		f: F,
	) -> Option<Rc<ParsedScriptFile>>
	where
		F: FnOnce(&mut Self) -> Option<Rc<ParsedScriptFile>>,
	{
		let key = (parents.clone(), file_path.to_string());
		if let Some(v) = self.cache_subset.get(&key) {
			return v.clone();
		}
		let v = f(self);
		self.cache_subset.insert(key, v.clone());
		v
	}

	#[cfg(test)]
	pub(crate) fn cache_size(&self) -> usize {
		self.cache_subset.len()
	}
}

fn resolved_patches(resolutions: &[PatchResolution]) -> Vec<super::patch::ClausewitzPatch> {
	resolutions
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(patch) => Some(patch.clone()),
			PatchResolution::AutoMerged { result, .. } => Some(result.clone()),
			PatchResolution::Conflict { .. } => None,
		})
		.collect()
}

fn template_for<'a>(
	file_dag: &FileDag,
	vanilla: Option<&'a ParsedScriptFile>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
) -> Option<&'a ParsedScriptFile> {
	vanilla.or_else(|| {
		file_dag
			.contributors()
			.iter()
			.find_map(|mod_id| contributors.get(mod_id))
	})
}

fn synthesized_parsed_file(
	file_path: &str,
	template: Option<&ParsedScriptFile>,
	statements: Vec<AstStatement>,
) -> ParsedScriptFile {
	let path = PathBuf::from(file_path);
	let mut parsed = template.cloned().unwrap_or_else(|| ParsedScriptFile {
		mod_id: "__foch_dag_base__".to_string(),
		path: path.clone(),
		relative_path: path.clone(),
		content_family: None,
		file_kind: ScriptFileKind::Other,
		module_name: "dag_base".to_string(),
		ast: AstFile {
			path: path.clone(),
			statements: Vec::new(),
		},
		source: String::new(),
		parse_issues: Vec::new(),
		parse_cache_hit: false,
	});
	parsed.mod_id = "__foch_dag_base__".to_string();
	parsed.path = path.clone();
	parsed.relative_path = path.clone();
	parsed.ast.path = path;
	parsed.ast.statements = statements;
	parsed.source.clear();
	parsed.parse_issues.clear();
	parsed.parse_cache_hit = false;
	parsed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use std::path::PathBuf;

	fn mod_with(
		mod_id: &str,
		name: &str,
		dependencies: Vec<&str>,
		replace_path: Vec<&str>,
	) -> ModCandidate {
		let descriptor = ModDescriptor {
			name: name.to_string(),
			dependencies: dependencies.into_iter().map(str::to_string).collect(),
			replace_path: replace_path.into_iter().map(str::to_string).collect(),
			..ModDescriptor::default()
		};
		let entry = PlaylistEntry {
			steam_id: Some(mod_id.to_string()),
			..PlaylistEntry::default()
		};
		ModCandidate {
			entry,
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(descriptor),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn mid(s: &str) -> ModId {
		ModId(s.to_string())
	}

	fn file_contributor(mod_id: &str, precedence: usize) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/common/foo.txt")),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
		}
	}

	fn parsed_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/foo.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: ScriptFileKind::Other,
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_inventory(entries: &[(&str, &str)]) -> HashMap<ModId, ParsedScriptFile> {
		entries
			.iter()
			.map(|(mod_id, source)| (mid(mod_id), parsed_file(mod_id, source)))
			.collect()
	}

	fn top_level_keys(parsed: &ParsedScriptFile) -> Vec<String> {
		let mut keys: Vec<_> = parsed
			.ast
			.statements
			.iter()
			.filter_map(|stmt| match stmt {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn computed_base_keys(
		resolver: &mut BaseResolver,
		parents: &BTreeSet<ModId>,
		fdag: &FileDag,
		vanilla: Option<&ParsedScriptFile>,
		inventory: &HashMap<ModId, ParsedScriptFile>,
	) -> Vec<String> {
		let merged = resolver
			.compute_merged_base(
				parents,
				fdag,
				vanilla,
				inventory,
				MergeKeySource::AssignmentKey,
				&MergePolicies::default(),
			)
			.expect("merged base");
		top_level_keys(&merged)
	}

	// -----------------------------------------------------------------------
	// Design §F.1 cases
	// -----------------------------------------------------------------------

	#[test]
	fn independent_mods_vs_vanilla() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		assert_eq!(dag.parents_of(&mid("a")), &[] as &[ModId]);
		assert_eq!(dag.parents_of(&mid("b")), &[] as &[ModId]);
		assert_eq!(dag.parents_of(&mid("c")), &[] as &[ModId]);

		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		for m in ["a", "b", "c"] {
			let r = resolver.resolve_base(&fdag, &mid(m));
			assert_eq!(r.kind, BaseSourceKind::Vanilla, "{m}");
			assert!(r.parents.is_empty());
		}
	}

	#[test]
	fn single_dep() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		assert_eq!(dag.parents_of(&mid("b")), &[mid("a")]);
		assert_eq!(dag.children_of(&mid("a")), &[mid("b")]);

		let contribs = vec![file_contributor("a", 1), file_contributor("b", 2)];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		assert_eq!(
			resolver.resolve_base(&fdag, &mid("a")).kind,
			BaseSourceKind::Vanilla
		);
		let rb = resolver.resolve_base(&fdag, &mid("b"));
		assert_eq!(rb.kind, BaseSourceKind::Synthesized);
		assert_eq!(rb.parents, BTreeSet::from([mid("a")]));
	}

	#[test]
	fn two_deps_diamond() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("d", "D", vec!["A", "B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		// D's parents are A and B (in declaration order).
		assert_eq!(dag.parents_of(&mid("d")), &[mid("a"), mid("b")]);

		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("d", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		let rb = resolver.resolve_base(&fdag, &mid("d"));
		assert_eq!(rb.kind, BaseSourceKind::Synthesized);
		assert_eq!(rb.parents, BTreeSet::from([mid("a"), mid("b")]));
	}

	#[test]
	fn transitive_chain() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
			mod_with("c", "C", vec!["B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		// Topo order respects parents-before-children.
		let topo = dag.topo();
		let pos = |m: &str| topo.iter().position(|x| x == &mid(m)).unwrap();
		assert!(pos("a") < pos("b"));
		assert!(pos("b") < pos("c"));

		// Per-file: C's induced parents include only B (direct); A is
		// transitive through B and not a direct parent in the file DAG.
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		assert_eq!(fdag.parents_of(&mid("c")), &[mid("b")]);
		assert_eq!(fdag.parents_of(&mid("b")), &[mid("a")]);
		assert_eq!(fdag.parents_of(&mid("a")), &[] as &[ModId]);
	}

	#[test]
	fn cycle_break() {
		let mods = vec![
			mod_with("a", "A", vec!["B"], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		// Diagnostics should contain a cycle warning + a break-edge info.
		let has_cycle = diags
			.iter()
			.any(|d| matches!(d.kind, DagDiagnosticKind::DependencyCycle { .. }));
		let has_break = diags
			.iter()
			.any(|d| matches!(d.kind, DagDiagnosticKind::BrokenCycleEdge { .. }));
		assert!(has_cycle, "expected cycle diagnostic, got {:?}", diags);
		assert!(
			has_break,
			"expected broken-edge diagnostic, got {:?}",
			diags
		);
		// The higher-position child (b) is the victim → the edge B→A is
		// dropped, leaving A→B intact.
		assert_eq!(dag.parents_of(&mid("a")), &[mid("b")]);
		assert_eq!(dag.parents_of(&mid("b")), &[] as &[ModId]);
	}

	#[test]
	fn dep_not_in_playset() {
		let mods = vec![mod_with("b", "B", vec!["Ghost"], vec![])];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(matches!(
			diags[0].kind,
			DagDiagnosticKind::MissingDependency { .. }
		));
		assert_eq!(dag.parents_of(&mid("b")), &[] as &[ModId]);
		assert_eq!(dag.missing_deps().len(), 1);
	}

	#[test]
	fn dep_does_not_ship_file() {
		// B depends on A; only B ships the file.
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
		];
		let (dag, _diags) = build_mod_dag(&mods);
		// Only B is in the file inventory.
		let contribs = vec![file_contributor("b", 2)];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		// A is not a contributor → B's per-file parents are empty →
		// B's base = Vanilla.
		assert_eq!(fdag.parents_of(&mid("b")), &[] as &[ModId]);
		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		assert_eq!(
			resolver.resolve_base(&fdag, &mid("b")).kind,
			BaseSourceKind::Vanilla
		);
	}

	#[test]
	fn replace_path_drops_priors() {
		// C declares replace_path="common"; A and B (earlier precedence)
		// also ship common/foo.txt → they're dropped.
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec!["common"]),
		];
		let (dag, _diags) = build_mod_dag(&mods);
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		// Only C remains as a contributor.
		assert_eq!(fdag.contributors(), &[mid("c")]);
		assert!(fdag.replaces_path(&mid("c")));

		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		assert_eq!(
			resolver.resolve_base(&fdag, &mid("c")).kind,
			BaseSourceKind::Empty
		);
	}

	#[test]
	fn replace_path_ignored_when_overridden() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec!["common"]),
		];
		let (dag, _diags) = build_mod_dag(&mods);
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		// All replace_path overrides → priors stay.
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::All);
		assert_eq!(fdag.contributors(), &[mid("a"), mid("b"), mid("c")]);
		assert!(!fdag.replaces_path(&mid("c")));
	}

	#[test]
	fn name_alternates() {
		// B declares two names; both resolve. (Here only one is present
		// in the playset; the other is a missing-dep diagnostic.)
		let mods = vec![
			mod_with("me_new", "Missions Expanded", vec![], vec![]),
			mod_with(
				"b",
				"B",
				vec!["Missions Expanded", "Missions Expanded (old)"],
				vec![],
			),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert_eq!(dag.parents_of(&mid("b")), &[mid("me_new")]);
		// The "(old)" alternate is missing → one diagnostic.
		assert_eq!(
			diags
				.iter()
				.filter(|d| matches!(d.kind, DagDiagnosticKind::MissingDependency { .. }))
				.count(),
			1
		);

		// Now both alternates present → both edges, deduped if same target.
		let mods = vec![
			mod_with("me_new", "Missions Expanded", vec![], vec![]),
			mod_with("me_old", "Missions Expanded (old)", vec![], vec![]),
			mod_with(
				"b",
				"B",
				vec!["Missions Expanded", "Missions Expanded (old)"],
				vec![],
			),
		];
		let (dag, _) = build_mod_dag(&mods);
		assert_eq!(dag.parents_of(&mid("b")), &[mid("me_new"), mid("me_old")]);
	}

	#[test]
	fn chain_regression_achievements() {
		// Reproduces the 371-conflict pattern: mod_a (Europa Expanded) ships
		// achievements.txt, mod_b independently ships an effectively-vanilla
		// achievements.txt, and there is no declared dep between them.
		// Today's chain forces mod_b to diff against mod_a → 371 phantom
		// removes. Under DAG: mod_b's per-file parents are empty → base =
		// Vanilla, no spurious diff.
		let mods = vec![
			mod_with("ee", "Europa Expanded", vec![], vec![]),
			mod_with("bx", "Independent Mod B", vec![], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![file_contributor("ee", 1), file_contributor("bx", 2)];
		let fdag = induced_file_dag(
			&dag,
			"common/achievements.txt",
			&contribs,
			&IgnoreReplacePath::None,
		);
		let resolver = BaseResolver::new(IgnoreReplacePath::None);
		assert_eq!(
			resolver.resolve_base(&fdag, &mid("ee")).kind,
			BaseSourceKind::Vanilla
		);
		// The headline assertion: mod_b's base is vanilla, NOT mod_a.
		let rb = resolver.resolve_base(&fdag, &mid("bx"));
		assert_eq!(rb.kind, BaseSourceKind::Vanilla);
		assert!(rb.parents.is_empty());
	}

	#[test]
	fn recursive_base_two_deep_chain_includes_transitive_parent() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
			mod_with("c", "C", vec!["B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let vanilla = parsed_file("__game__", "root = yes\n");
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("b", "root = yes\na = yes\nb = yes\n"),
			("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
		]);
		let mut resolver = BaseResolver::new(IgnoreReplacePath::None);
		let c_base = resolver.resolve_base(&fdag, &mid("c"));
		assert_eq!(c_base.parents, BTreeSet::from([mid("a"), mid("b")]));

		let keys = computed_base_keys(
			&mut resolver,
			&c_base.parents,
			&fdag,
			Some(&vanilla),
			&inventory,
		);
		assert_eq!(keys, vec!["a", "b", "root"]);
	}

	#[test]
	fn recursive_base_diamond_merges_shared_ancestor_once() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
			mod_with("c", "C", vec!["A"], vec![]),
			mod_with("d", "D", vec!["B", "C"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
			file_contributor("d", 4),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let vanilla = parsed_file("__game__", "root = yes\n");
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("b", "root = yes\na = yes\nb = yes\n"),
			("c", "root = yes\na = yes\nc = yes\n"),
			("d", "root = yes\na = yes\nb = yes\nc = yes\nd = yes\n"),
		]);
		let mut resolver = BaseResolver::new(IgnoreReplacePath::None);
		let d_base = resolver.resolve_base(&fdag, &mid("d"));
		assert_eq!(
			d_base.parents,
			BTreeSet::from([mid("a"), mid("b"), mid("c")])
		);

		let keys = computed_base_keys(
			&mut resolver,
			&d_base.parents,
			&fdag,
			Some(&vanilla),
			&inventory,
		);
		assert_eq!(keys, vec!["a", "b", "c", "root"]);
	}

	#[test]
	fn recursive_base_lifts_through_missing_file_dep() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec![]),
			mod_with("c", "C", vec!["B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![file_contributor("a", 1), file_contributor("c", 3)];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let vanilla = parsed_file("__game__", "root = yes\n");
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("c", "root = yes\na = yes\nc = yes\n"),
		]);
		let mut resolver = BaseResolver::new(IgnoreReplacePath::None);
		let c_base = resolver.resolve_base(&fdag, &mid("c"));
		assert_eq!(c_base.parents, BTreeSet::from([mid("a")]));

		let keys = computed_base_keys(
			&mut resolver,
			&c_base.parents,
			&fdag,
			Some(&vanilla),
			&inventory,
		);
		assert_eq!(keys, vec!["a", "root"]);
	}

	#[test]
	fn replace_path_parent_forces_empty_recursive_foundation() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec!["common"]),
			mod_with("c", "C", vec!["B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		assert_eq!(fdag.contributors(), &[mid("b"), mid("c")]);
		assert!(fdag.replaces_path(&mid("b")));
		let vanilla = parsed_file("__game__", "root = yes\n");
		let inventory = parsed_inventory(&[("b", "b = yes\n"), ("c", "b = yes\nc = yes\n")]);
		let mut resolver = BaseResolver::new(IgnoreReplacePath::None);
		let c_base = resolver.resolve_base(&fdag, &mid("c"));
		assert_eq!(c_base.parents, BTreeSet::from([mid("b")]));

		let keys = computed_base_keys(
			&mut resolver,
			&c_base.parents,
			&fdag,
			Some(&vanilla),
			&inventory,
		);
		assert_eq!(keys, vec!["b"]);
	}

	#[test]
	fn ignore_replace_path_restores_recursive_vanilla_foundation() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec!["A"], vec!["common"]),
			mod_with("c", "C", vec!["B"], vec![]),
		];
		let (dag, diags) = build_mod_dag(&mods);
		assert!(diags.is_empty());
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::All);
		assert_eq!(fdag.contributors(), &[mid("a"), mid("b"), mid("c")]);
		assert!(!fdag.replaces_path(&mid("b")));
		let vanilla = parsed_file("__game__", "root = yes\n");
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("b", "root = yes\na = yes\nb = yes\n"),
			("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
		]);
		let mut resolver = BaseResolver::new(IgnoreReplacePath::All);
		let c_base = resolver.resolve_base(&fdag, &mid("c"));
		assert_eq!(c_base.parents, BTreeSet::from([mid("a"), mid("b")]));

		let keys = computed_base_keys(
			&mut resolver,
			&c_base.parents,
			&fdag,
			Some(&vanilla),
			&inventory,
		);
		assert_eq!(keys, vec!["a", "b", "root"]);
	}

	// -----------------------------------------------------------------------
	// Memoization / determinism / replace_path-ignore extras
	// -----------------------------------------------------------------------

	#[test]
	fn memoization_dedupes_compute_calls() {
		let mut resolver = BaseResolver::new(IgnoreReplacePath::None);
		let parents: BTreeSet<ModId> = BTreeSet::from([mid("a"), mid("b")]);
		let count = std::cell::Cell::new(0u32);
		let _ = resolver.merged_base_or_compute(&parents, "common/foo.txt", |_| {
			count.set(count.get() + 1);
			None
		});
		let _ = resolver.merged_base_or_compute(&parents, "common/foo.txt", |_| {
			count.set(count.get() + 1);
			None
		});
		assert_eq!(count.get(), 1, "second call must hit the cache");
		assert_eq!(resolver.cache_size(), 1);

		// Different parent set → separate compute.
		let other: BTreeSet<ModId> = BTreeSet::from([mid("a")]);
		let _ = resolver.merged_base_or_compute(&other, "common/foo.txt", |_| {
			count.set(count.get() + 1);
			None
		});
		assert_eq!(count.get(), 2);

		// Different file path → separate compute.
		let _ = resolver.merged_base_or_compute(&parents, "common/bar.txt", |_| {
			count.set(count.get() + 1);
			None
		});
		assert_eq!(count.get(), 3);
	}

	#[test]
	fn deterministic_ordering_across_runs() {
		// Build a non-trivial DAG twice in identical order; the topo and
		// children lists must match exactly (no HashMap iteration leaking
		// into output).
		let make_mods = || {
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
				mod_with("d", "D", vec!["B", "C"], vec![]),
			]
		};
		let (dag1, _) = build_mod_dag(&make_mods());
		let (dag2, _) = build_mod_dag(&make_mods());
		assert_eq!(dag1.topo(), dag2.topo());
		for m in [mid("a"), mid("b"), mid("c"), mid("d")] {
			assert_eq!(dag1.parents_of(&m), dag2.parents_of(&m));
			assert_eq!(dag1.children_of(&m), dag2.children_of(&m));
		}
		// children_of(A) must be deterministic [B, C] (playlist order).
		assert_eq!(dag1.children_of(&mid("a")), &[mid("b"), mid("c")]);

		// Same FileDag determinism.
		let mods = make_mods();
		let (dag, _) = build_mod_dag(&mods);
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
			file_contributor("d", 4),
		];
		let f1 = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		let f2 = induced_file_dag(&dag, "common/foo.txt", &contribs, &IgnoreReplacePath::None);
		assert_eq!(f1.contributors(), f2.contributors());
		for m in [mid("a"), mid("b"), mid("c"), mid("d")] {
			assert_eq!(f1.parents_of(&m), f2.parents_of(&m));
		}
	}
}
