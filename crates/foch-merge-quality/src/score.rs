//! Scoring: run `foch merge` on a synthetic 2-mod playset and classify, for
//! every file the compatch hand-merged (ground truth), how foch's structural
//! merge compares — structurally and by line similarity.
//!
//! This is a faithful port of the Python harness's scoring so the verdicts are
//! identical, with one deliberate change: the merge runs **in-process** via
//! `foch_engine::run_merge_with_options` (no `foch` subprocess) with
//! `include_game_base = false` and a ground-truth retained-path set — the
//! comparable target for a self-contained compatch without materializing or
//! parsing unrelated mod files.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use foch_core::model::MergeReport;
use foch_engine::{
	CheckRequest, Config, MergeError, MergeExecuteOptions, MergeExecutionResult,
	run_merge_with_options,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentFamilyPathMatcher, GameProfile, MergeKeySource, ModuleNameRule,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, parse_clausewitz_file};
use regex::Regex;

/// `^key = {` at a line start — a top-level Clausewitz definition.
static TOP_KEY_RE: LazyLock<Regex> =
	LazyLock::new(|| Regex::new(r"(?m)^([A-Za-z_][\w.\-]*)\s*=\s*\{").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
/// `for <path>;` inside a conflict warning string.
static WARN_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"for ([\w./\-]+);").unwrap());

const SKIP_NAMES: &[&str] = &["descriptor.mod", "thumbnail.png"];
const SKIP_EXTS: &[&str] = &["bak", "jpg", "jpeg", "png", "dds", "tga", "mod"];

/// Read a file as UTF-8, replacing invalid sequences (mirrors Python's
/// `read_text(errors="replace")`). Returns `None` only on I/O error.
pub fn read(path: &Path) -> Option<String> {
	match fs::read(path) {
		Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
		Err(_) => None,
	}
}

/// Set of top-level definition keys in a Clausewitz file.
pub fn top_level_keys(text: &str) -> HashSet<String> {
	TOP_KEY_RE
		.captures_iter(text)
		.map(|c| c[1].to_string())
		.collect()
}

/// Whitespace/comment-insensitive line list for similarity scoring.
fn normalise(text: &str) -> Vec<String> {
	text.lines()
		.filter_map(|line| {
			let stripped = line.split('#').next().unwrap_or("").trim();
			let collapsed = WS_RE.replace_all(stripped, " ").into_owned();
			if collapsed.is_empty() {
				None
			} else {
				Some(collapsed)
			}
		})
		.collect()
}

/// `difflib.SequenceMatcher(None, normalise(a), normalise(b)).ratio()`.
pub fn similarity(a: &str, b: &str) -> f64 {
	let la = normalise(a);
	let lb = normalise(b);
	ratio(&la, &lb)
}

// --- faithful port of CPython difflib.SequenceMatcher.ratio() over lines ---
// autojunk is irrelevant for our line counts (it only triggers for sequences
// longer than 200 elements with elements appearing in >1% of positions).

fn ratio(a: &[String], b: &[String]) -> f64 {
	let total = a.len() + b.len();
	if total == 0 {
		return 1.0;
	}
	let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
	for (j, s) in b.iter().enumerate() {
		b2j.entry(s.as_str()).or_default().push(j);
	}
	// Sum matching-block sizes the way CPython's get_matching_blocks does —
	// iteratively over an explicit queue of ranges, NOT recursively. Recursion
	// here overflows the stack on long files (deep left/right splits); the queue
	// gives the identical total with bounded stack.
	let mut matches = 0usize;
	let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
	while let Some((alo, ahi, blo, bhi)) = queue.pop() {
		let (i, j, k) = find_longest_match(a, &b2j, alo, ahi, blo, bhi);
		if k == 0 {
			continue;
		}
		matches += k;
		if alo < i && blo < j {
			queue.push((alo, i, blo, j));
		}
		if i + k < ahi && j + k < bhi {
			queue.push((i + k, ahi, j + k, bhi));
		}
	}
	2.0 * matches as f64 / total as f64
}

fn find_longest_match(
	a: &[String],
	b2j: &HashMap<&str, Vec<usize>>,
	alo: usize,
	ahi: usize,
	blo: usize,
	bhi: usize,
) -> (usize, usize, usize) {
	let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
	let mut j2len: HashMap<usize, usize> = HashMap::new();
	for (offset, item) in a[alo..ahi].iter().enumerate() {
		let i = alo + offset;
		let mut newj2len: HashMap<usize, usize> = HashMap::new();
		if let Some(js) = b2j.get(item.as_str()) {
			for &j in js {
				if j < blo {
					continue;
				}
				if j >= bhi {
					break;
				}
				let k = j2len.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
				newj2len.insert(j, k);
				if k > bestsize {
					besti = i + 1 - k;
					bestj = j + 1 - k;
					bestsize = k;
				}
			}
		}
		j2len = newj2len;
	}
	(besti, bestj, bestsize)
}

/// Relative paths of every file the compatch hand-merged (the ground-truth set),
/// skipping descriptors and non-script binary assets.
pub fn ground_truth_files(compatch_dir: &Path) -> Vec<String> {
	let mut out = Vec::new();
	for entry in walkdir::WalkDir::new(compatch_dir)
		.into_iter()
		.filter_map(Result::ok)
	{
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
		if SKIP_NAMES.contains(&name) {
			continue;
		}
		let ext = path
			.extension()
			.and_then(|e| e.to_str())
			.map(str::to_ascii_lowercase)
			.unwrap_or_default();
		if SKIP_EXTS.contains(&ext.as_str()) {
			continue;
		}
		if let Ok(rel) = path.strip_prefix(compatch_dir) {
			out.push(rel.to_string_lossy().replace('\\', "/"));
		}
	}
	out.sort();
	out
}

/// Syntactically index a mod's top-level definitions by `(content_directory,
/// key)` -> the relative paths of the `.txt` files that define them.
///
/// This is a deliberately schema-free index for full-local symbol reports. It
/// does not claim visibility or conflict authority; it only answers "which mod
/// files define the same top-level key in the same content directory?"
/// Restricted to `.txt`; `.gui`/`.gfx`/`.yml` are handled by file-path overlap.
pub fn definition_index(mod_dir: &Path) -> HashMap<(String, String), Vec<String>> {
	let mut index: HashMap<(String, String), Vec<String>> = HashMap::new();
	for entry in walkdir::WalkDir::new(mod_dir)
		.into_iter()
		.filter_map(Result::ok)
	{
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		if path.extension().and_then(|e| e.to_str()) != Some("txt") {
			continue;
		}
		let Ok(rel) = path.strip_prefix(mod_dir) else {
			continue;
		};
		let rel_s = rel.to_string_lossy().replace('\\', "/");
		let dir = rel
			.parent()
			.map(|p| p.to_string_lossy().replace('\\', "/"))
			.unwrap_or_default();
		if let Some(text) = read(path) {
			for key in top_level_keys(&text) {
				index
					.entry((dir.clone(), key))
					.or_default()
					.push(rel_s.clone());
			}
		}
	}
	index
}

/// Paths foch surfaced as conflicts (it declined to auto-merge; a human did).
pub fn conflict_rel_paths(report: &MergeReport) -> HashSet<String> {
	let mut out = HashSet::new();
	for c in &report.conflict_resolutions {
		if !c.path.is_empty() {
			out.insert(c.path.clone());
		}
	}
	for w in &report.warnings {
		if let Some(m) = WARN_PATH_RE.captures(w) {
			out.insert(m[1].to_string());
		}
	}
	out
}

/// Write `dlc_load.json` + `mod/ugc_<id>.mod` descriptors pointing at mod dirs.
pub fn write_playset(tmp: &Path, mods: &[(String, PathBuf)]) -> io::Result<PathBuf> {
	fs::create_dir_all(tmp.join("mod"))?;
	let mut enabled = Vec::new();
	for (steam_id, ws_dir) in mods {
		let rel = format!("mod/ugc_{steam_id}.mod");
		let path_val = ws_dir.to_string_lossy().replace('\\', "/");
		fs::write(
			tmp.join(&rel),
			format!("name=\"{steam_id}\"\npath=\"{path_val}\"\nremote_file_id=\"{steam_id}\"\n"),
		)?;
		enabled.push(rel);
	}
	let dlc = serde_json::json!({ "enabled_mods": enabled, "disabled_dlcs": [] });
	let dlc_path = tmp.join("dlc_load.json");
	fs::write(&dlc_path, serde_json::to_string(&dlc).unwrap())?;
	Ok(dlc_path)
}

/// Stack size for the merge worker thread. foch's merge planner recurses on
/// definition nesting; deeply-nested community mod files can exceed the default
/// ~8 MB main-thread stack (macOS caps it low), so we run the merge on a worker
/// thread with a generous stack — a pathological file then degrades to a slow
/// merge rather than aborting the whole harness process.
const MERGE_STACK_BYTES: usize = 512 * 1024 * 1024;

/// Run a merge of `playset` into `out_dir`, in-process, with the game base
/// excluded. When `retained_paths` is set, merge planning and output are
/// limited to that ground-truth set. `force` auto-resolves manual conflicts
/// when true; when false, conflicting files are withheld and surface in the
/// report.
///
/// Runs on a large-stack worker thread (see [`MERGE_STACK_BYTES`]).
pub fn run_merge(
	playset: &Path,
	out_dir: &Path,
	force: bool,
	retained_paths: Option<BTreeSet<String>>,
) -> Result<MergeExecutionResult, MergeError> {
	let playset = playset.to_path_buf();
	let out_dir = out_dir.to_path_buf();
	std::thread::Builder::new()
		.stack_size(MERGE_STACK_BYTES)
		.spawn(move || run_merge_inner(&playset, &out_dir, force, retained_paths))
		.expect("spawn merge worker thread")
		.join()
		.expect("merge worker thread panicked")
}

fn run_merge_inner(
	playset: &Path,
	out_dir: &Path,
	force: bool,
	retained_paths: Option<BTreeSet<String>>,
) -> Result<MergeExecutionResult, MergeError> {
	run_merge_with_options(
		CheckRequest::from_playset_path(
			playset.to_path_buf(),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path: HashMap::new(),
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeExecuteOptions {
			out_dir: out_dir.to_path_buf(),
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			retained_paths,
		},
	)
}

/// Classification of foch's output for one ground-truth file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
	/// foch surfaced it as a conflict; the human resolved by hand.
	ConflictWithheld,
	/// foch emitted nothing for this path.
	NotEmitted,
	/// same definitions, line-similarity ≥ 0.92 to the human merge.
	MatchesHuman,
	/// same parsed AST under the corpus ordering policy, but different text.
	MatchesAst,
	/// differs from the human AST under strict comparison, but is accepted by an
	/// explicit corpus equivalence policy.
	AcceptedEquivalent,
	/// differs from the human AST, but a committed adjudication accepts foch's
	/// output as better than the human compatch for this file.
	AcceptedBetter,
	/// foch dropped top-level definitions present in either input mod.
	DropsContent,
	/// AST comparison was unavailable; same definitions as the human, different text.
	DivergesFormatting,
	/// same top-level definitions as the human, but different parsed AST.
	DivergesAst,
	/// different top-level definitions from the human merge.
	DivergesStructure,
}

impl Verdict {
	pub fn as_str(self) -> &'static str {
		match self {
			Verdict::ConflictWithheld => "conflict_withheld",
			Verdict::NotEmitted => "not_emitted",
			Verdict::MatchesHuman => "matches_human",
			Verdict::MatchesAst => "matches_ast",
			Verdict::AcceptedEquivalent => "accepted_equivalent",
			Verdict::AcceptedBetter => "accepted_better",
			Verdict::DropsContent => "drops_content",
			Verdict::DivergesFormatting => "diverges_formatting",
			Verdict::DivergesAst => "diverges_ast",
			Verdict::DivergesStructure => "diverges_structure",
		}
	}

	pub fn accepted_ok(self) -> bool {
		matches!(
			self,
			Verdict::MatchesHuman
				| Verdict::MatchesAst
				| Verdict::AcceptedEquivalent
				| Verdict::AcceptedBetter
		)
	}
}

#[derive(Clone, Debug, Default)]
pub struct Adjudications {
	records: HashMap<(String, String), AcceptedAdjudication>,
}

impl Adjudications {
	pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
		let records: Vec<AcceptedAdjudicationRecord> = serde_json::from_str(text)?;
		let records = records
			.into_iter()
			.map(|record| {
				(
					(record.compatch_id, record.rel),
					AcceptedAdjudication {
						verdict: record.verdict,
						reason: record.reason,
					},
				)
			})
			.collect();
		Ok(Self { records })
	}

	pub fn built_in() -> Self {
		Self::from_json(include_str!("../tests/fixtures/adjudications.json"))
			.expect("built-in adjudications fixture parses")
	}

	fn get(&self, compatch_id: &str, rel: &str) -> Option<&AcceptedAdjudication> {
		self.records
			.get(&(compatch_id.to_string(), rel.to_string()))
	}
}

#[derive(Clone, Debug, serde::Deserialize)]
struct AcceptedAdjudicationRecord {
	compatch_id: String,
	rel: String,
	verdict: AcceptedAdjudicationVerdict,
	reason: String,
}

#[derive(Clone, Debug)]
struct AcceptedAdjudication {
	verdict: AcceptedAdjudicationVerdict,
	reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AcceptedAdjudicationVerdict {
	AcceptedBetter,
}

#[derive(Clone, Debug)]
pub struct FileScore {
	pub rel: String,
	pub in_a: bool,
	pub in_b: bool,
	pub overlap: bool,
	pub foch_emitted: bool,
	pub foch_conflict: bool,
	pub similarity: Option<f64>,
	pub keys_match: Option<bool>,
	pub ast_match: Option<bool>,
	pub dropped_keys: Vec<String>,
	pub verdict: Verdict,
	pub acceptance_reason: Option<String>,
}

pub struct ScoreFileRequest<'a> {
	pub compatch_id: &'a str,
	pub rel: &'a str,
	pub mod_a: &'a Path,
	pub mod_b: &'a Path,
	pub compatch: &'a Path,
	pub out_dir: &'a Path,
	pub basegame_root: Option<&'a Path>,
	pub conflict_paths: &'a HashSet<String>,
	pub adjudications: &'a Adjudications,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ContentKey(String);

struct ContentEntry {
	text: String,
	normalized: Option<Vec<String>>,
	keys: Option<HashSet<String>>,
	canonical: HashMap<(String, AstOrderingPolicy), Option<Vec<CanonicalStatement>>>,
}

impl ContentEntry {
	fn new(text: String) -> Self {
		Self {
			text,
			normalized: None,
			keys: None,
			canonical: HashMap::new(),
		}
	}
}

#[derive(Default)]
pub struct ScoreCache {
	path_content: HashMap<PathBuf, Option<ContentKey>>,
	content_entries: HashMap<ContentKey, ContentEntry>,
	module_views: HashMap<(PathBuf, String), Option<BTreeMap<String, CanonicalStatement>>>,
}

impl ScoreCache {
	pub fn new() -> Self {
		Self::default()
	}

	fn content_key(&mut self, path: &Path) -> Option<ContentKey> {
		let path = path.to_path_buf();
		if !self.path_content.contains_key(&path) {
			let content = match fs::read(&path) {
				Ok(bytes) => {
					let hash = blake3::hash(&bytes).to_hex().to_string();
					let key = ContentKey(hash);
					self.content_entries.entry(key.clone()).or_insert_with(|| {
						ContentEntry::new(String::from_utf8_lossy(&bytes).into_owned())
					});
					Some(key)
				}
				Err(_) => None,
			};
			self.path_content.insert(path.clone(), content);
		}
		self.path_content
			.get(&path)
			.expect("path content cache inserted")
			.clone()
	}

	fn content_entry(&mut self, path: &Path) -> Option<&mut ContentEntry> {
		let key = self.content_key(path)?;
		Some(
			self.content_entries
				.get_mut(&key)
				.expect("content entry inserted"),
		)
	}

	fn normalized_lines(&mut self, path: &Path) -> Vec<String> {
		let Some(entry) = self.content_entry(path) else {
			return Vec::new();
		};
		if entry.normalized.is_none() {
			entry.normalized = Some(normalise(&entry.text));
		}
		entry
			.normalized
			.as_ref()
			.expect("normalized lines inserted")
			.clone()
	}

	fn top_level_keys(&mut self, path: &Path) -> HashSet<String> {
		let Some(entry) = self.content_entry(path) else {
			return HashSet::new();
		};
		if entry.keys.is_none() {
			entry.keys = Some(top_level_keys(&entry.text));
		}
		entry
			.keys
			.as_ref()
			.expect("top-level keys inserted")
			.clone()
	}

	fn rounded_similarity(&mut self, left: &Path, right: &Path) -> Option<f64> {
		if !left.is_file() || !right.is_file() {
			return None;
		}
		let left_lines = self.normalized_lines(left);
		let right_lines = self.normalized_lines(right);
		Some((ratio(&left_lines, &right_lines) * 1000.0).round() / 1000.0)
	}

	fn canonical_ast(
		&mut self,
		rel: &str,
		path: &Path,
		ordering: AstOrderingPolicy,
	) -> Option<Vec<CanonicalStatement>> {
		if !is_clausewitz_like_path(rel) {
			return None;
		}
		let content = self.content_key(path)?;
		let key = (syntax_cache_key(path), ordering);
		if !self
			.content_entries
			.get(&content)
			.expect("content entry inserted")
			.canonical
			.contains_key(&key)
		{
			let parsed = parse_clausewitz_file(path);
			let canonical = if parsed.diagnostics.is_empty() {
				Some(canonical_statements(&parsed.ast.statements, ordering))
			} else {
				None
			};
			self.content_entries
				.get_mut(&content)
				.expect("content entry inserted")
				.canonical
				.insert(key.clone(), canonical);
		}
		self.content_entries
			.get(&content)
			.expect("content entry inserted")
			.canonical
			.get(&key)
			.expect("canonical AST inserted")
			.clone()
	}

	fn module_view(
		&mut self,
		root: &Path,
		family_prefix: &str,
	) -> Option<BTreeMap<String, CanonicalStatement>> {
		let key = (root.to_path_buf(), family_prefix.to_string());
		if !self.module_views.contains_key(&key) {
			let view = canonical_module_view_uncached(root, family_prefix);
			self.module_views.insert(key.clone(), view);
		}
		self.module_views
			.get(&key)
			.expect("module view inserted")
			.clone()
	}
}

/// Classify foch's merged output for one ground-truth file against the compatch.
pub fn score_file(request: &ScoreFileRequest<'_>) -> FileScore {
	let mut cache = ScoreCache::new();
	score_file_with_cache(request, &mut cache)
}

/// Classify foch's merged output, reusing parsed/text artifacts across files.
pub fn score_file_with_cache(request: &ScoreFileRequest<'_>, cache: &mut ScoreCache) -> FileScore {
	let rel = request.rel;
	let in_a = request.mod_a.join(rel).is_file();
	let in_b = request.mod_b.join(rel).is_file();
	let overlap = in_a && in_b;
	let foch_path = request.out_dir.join(rel);
	let foch_emitted = foch_path.is_file();
	let foch_conflict = request.conflict_paths.contains(rel);

	let compatch_path = request.compatch.join(rel);

	let mut sim = None;
	let mut keys_match = None;
	let mut ast_match = None;
	let mut policy_equivalent = false;
	let mut module_equivalent = false;
	let mut dropped: Vec<String> = Vec::new();
	if foch_emitted {
		let fk = cache.top_level_keys(&foch_path);
		let ck = cache.top_level_keys(&compatch_path);
		keys_match = Some(fk == ck);
		ast_match = ast_match_for_path_cached(cache, rel, &foch_path, &compatch_path);
		if ast_match == Some(true) {
			sim = cache.rounded_similarity(&foch_path, &compatch_path);
		}
		policy_equivalent = ast_match == Some(false)
			&& accepted_equivalent_for_path(cache, rel, &foch_path, &compatch_path);
		module_equivalent = ast_match != Some(true)
			&& keys_match != Some(true)
			&& same_family_module_equivalent(cache, rel, request.out_dir, request.compatch);
		let mod_a_keys = cache.top_level_keys(&request.mod_a.join(rel));
		let mod_b_keys = cache.top_level_keys(&request.mod_b.join(rel));
		let union_ab: HashSet<String> = mod_a_keys.union(&mod_b_keys).cloned().collect();
		dropped = union_ab.difference(&fk).cloned().collect();
		dropped.sort();
	}

	let (verdict, acceptance_reason) = if foch_conflict {
		(Verdict::ConflictWithheld, None)
	} else if !foch_emitted {
		(Verdict::NotEmitted, None)
	} else if ast_match == Some(true) && sim.is_some_and(|s| s >= 0.92) {
		(Verdict::MatchesHuman, None)
	} else if ast_match == Some(true) {
		(Verdict::MatchesAst, None)
	} else if policy_equivalent {
		(
			Verdict::AcceptedEquivalent,
			Some("gfx_order_insensitive_ast_equivalent".to_string()),
		)
	} else if module_equivalent {
		(
			Verdict::AcceptedEquivalent,
			Some("same_family_module_equivalent".to_string()),
		)
	} else if let Some(adjudication) = request.adjudications.get(request.compatch_id, rel) {
		match adjudication.verdict {
			AcceptedAdjudicationVerdict::AcceptedBetter => {
				(Verdict::AcceptedBetter, Some(adjudication.reason.clone()))
			}
		}
	} else if !dropped.is_empty() {
		(Verdict::DropsContent, None)
	} else if ast_match == Some(false) && keys_match == Some(true) {
		(Verdict::DivergesAst, None)
	} else if keys_match == Some(true) {
		(Verdict::DivergesFormatting, None)
	} else {
		(Verdict::DivergesStructure, None)
	};

	FileScore {
		rel: rel.to_string(),
		in_a,
		in_b,
		overlap,
		foch_emitted,
		foch_conflict,
		similarity: sim,
		keys_match,
		ast_match,
		dropped_keys: dropped,
		verdict,
		acceptance_reason,
	}
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AstOrderingPolicy {
	OrderSensitive,
	OrderInsensitive,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CanonicalValue {
	Scalar(String),
	Block(Vec<CanonicalStatement>),
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CanonicalStatement {
	Assignment { key: String, value: CanonicalValue },
	Item(CanonicalValue),
}

#[cfg(test)]
fn ast_match_for_path(rel: &str, foch_path: &Path, compatch_path: &Path) -> Option<bool> {
	let mut cache = ScoreCache::new();
	ast_match_for_path_cached(&mut cache, rel, foch_path, compatch_path)
}

fn ast_match_for_path_cached(
	cache: &mut ScoreCache,
	rel: &str,
	foch_path: &Path,
	compatch_path: &Path,
) -> Option<bool> {
	if !is_clausewitz_like_path(rel) {
		return None;
	}
	let ordering = if is_gui_like_path(rel) {
		AstOrderingPolicy::OrderSensitive
	} else {
		AstOrderingPolicy::OrderInsensitive
	};
	let foch = cache.canonical_ast(rel, foch_path, ordering)?;
	let compatch = cache.canonical_ast(rel, compatch_path, ordering)?;
	Some(foch == compatch)
}

fn accepted_equivalent_for_path(
	cache: &mut ScoreCache,
	rel: &str,
	foch_path: &Path,
	compatch_path: &Path,
) -> bool {
	if !is_gfx_path(rel) {
		return false;
	}
	ast_match_for_path_with_ordering_cached(
		cache,
		rel,
		foch_path,
		compatch_path,
		AstOrderingPolicy::OrderInsensitive,
	) == Some(true)
}

fn same_family_module_equivalent(
	cache: &mut ScoreCache,
	rel: &str,
	foch_root: &Path,
	compatch_root: &Path,
) -> bool {
	let Some(descriptor) = eligible_module_family(rel) else {
		return false;
	};
	let Some(prefix) = family_prefix(descriptor) else {
		return false;
	};
	let Some(foch) = cache.module_view(foch_root, prefix) else {
		return false;
	};
	let Some(compatch) = cache.module_view(compatch_root, prefix) else {
		return false;
	};
	foch == compatch
}

fn eligible_module_family(rel: &str) -> Option<&'static ContentFamilyDescriptor> {
	if is_path_sensitive_for_module_scoring(rel) {
		return None;
	}
	let descriptor = eu4_profile().classify_content_family(Path::new(rel))?;
	if !matches!(descriptor.matcher, ContentFamilyPathMatcher::Prefix(_)) {
		return None;
	}
	if !matches!(descriptor.module_name_rule, ModuleNameRule::Static(_)) {
		return None;
	}
	if descriptor.merge_key_source != Some(MergeKeySource::AssignmentKey) {
		return None;
	}
	Some(descriptor)
}

fn family_prefix(descriptor: &ContentFamilyDescriptor) -> Option<&'static str> {
	match descriptor.matcher {
		ContentFamilyPathMatcher::Prefix(prefix) => Some(prefix),
		ContentFamilyPathMatcher::Exact(_) => None,
	}
}

fn canonical_module_view_uncached(
	root: &Path,
	family_prefix: &str,
) -> Option<BTreeMap<String, CanonicalStatement>> {
	let family_dir = root.join(family_prefix);
	if !family_dir.is_dir() {
		return Some(BTreeMap::new());
	}
	let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&family_dir)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| entry.into_path())
		.filter(|path| module_view_includes_file(path))
		.collect();
	files.sort_by(|left, right| {
		relative_module_path(root, left).cmp(&relative_module_path(root, right))
	});

	let mut view = BTreeMap::new();
	for path in files {
		let parsed = parse_clausewitz_file(&path);
		if !parsed.diagnostics.is_empty() {
			return None;
		}
		for statement in &parsed.ast.statements {
			if let Some((key, canonical)) = canonical_module_assignment(statement) {
				view.insert(key, canonical);
			}
		}
	}
	Some(view)
}

fn module_view_includes_file(path: &Path) -> bool {
	let rel = path.to_string_lossy().replace('\\', "/");
	is_clausewitz_like_path(&rel)
		&& !is_gui_like_path(&rel)
		&& path.extension().and_then(|ext| ext.to_str()) == Some("txt")
}

fn relative_module_path(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
}

fn canonical_module_assignment(statement: &AstStatement) -> Option<(String, CanonicalStatement)> {
	let AstStatement::Assignment { key, value, .. } = statement else {
		return None;
	};
	Some((
		key.clone(),
		CanonicalStatement::Assignment {
			key: key.clone(),
			value: canonical_value(value, AstOrderingPolicy::OrderInsensitive),
		},
	))
}

fn ast_match_for_path_with_ordering_cached(
	cache: &mut ScoreCache,
	rel: &str,
	foch_path: &Path,
	compatch_path: &Path,
	ordering: AstOrderingPolicy,
) -> Option<bool> {
	if !is_clausewitz_like_path(rel) {
		return None;
	}
	let foch = cache.canonical_ast(rel, foch_path, ordering)?;
	let compatch = cache.canonical_ast(rel, compatch_path, ordering)?;
	Some(foch == compatch)
}

fn syntax_cache_key(path: &Path) -> String {
	path.extension()
		.and_then(|ext| ext.to_str())
		.unwrap_or_default()
		.to_ascii_lowercase()
}

fn is_clausewitz_like_path(rel: &str) -> bool {
	let lower = rel.to_ascii_lowercase();
	lower.ends_with(".txt")
		|| lower.ends_with(".gui")
		|| lower.ends_with(".gfx")
		|| lower.ends_with(".lua")
}

fn is_gui_like_path(rel: &str) -> bool {
	let lower = rel.to_ascii_lowercase();
	lower.starts_with("interface/")
		|| lower.starts_with("common/interface/")
		|| lower.starts_with("gfx/")
		|| lower.ends_with(".gui")
		|| lower.ends_with(".gfx")
}

fn is_gfx_path(rel: &str) -> bool {
	rel.to_ascii_lowercase().ends_with(".gfx")
}

fn is_path_sensitive_for_module_scoring(rel: &str) -> bool {
	let lower = rel.to_ascii_lowercase();
	is_gui_like_path(&lower)
		|| lower.starts_with("history/")
		|| lower.starts_with("map/")
		|| lower.starts_with("music/")
		|| lower.starts_with("sound/")
		|| lower.starts_with("tutorial/")
}

fn canonical_statements(
	statements: &[AstStatement],
	ordering: AstOrderingPolicy,
) -> Vec<CanonicalStatement> {
	let mut canonical = statements
		.iter()
		.filter_map(|statement| canonical_statement(statement, ordering))
		.collect::<Vec<_>>();
	if ordering == AstOrderingPolicy::OrderInsensitive {
		canonical.sort();
	}
	canonical
}

fn canonical_statement(
	statement: &AstStatement,
	ordering: AstOrderingPolicy,
) -> Option<CanonicalStatement> {
	match statement {
		AstStatement::Assignment { key, value, .. } => Some(CanonicalStatement::Assignment {
			key: key.clone(),
			value: canonical_value(value, ordering),
		}),
		AstStatement::Item { value, .. } => {
			Some(CanonicalStatement::Item(canonical_value(value, ordering)))
		}
		AstStatement::Comment { .. } => None,
	}
}

fn canonical_value(value: &AstValue, ordering: AstOrderingPolicy) -> CanonicalValue {
	match value {
		AstValue::Scalar { value, .. } => CanonicalValue::Scalar(canonical_scalar(value)),
		AstValue::Block { items, .. } => {
			CanonicalValue::Block(canonical_statements(items, ordering))
		}
	}
}

fn canonical_scalar(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) | ScalarValue::String(value)
			if is_valid_bare_identifier_text(value) =>
		{
			format!("text:{value}")
		}
		ScalarValue::Identifier(value) => format!("identifier:{value}"),
		ScalarValue::String(value) => format!("string:{value}"),
		ScalarValue::Number(value) => format!("number:{value}"),
		ScalarValue::Bool(value) => {
			if *value {
				"bool:yes".to_string()
			} else {
				"bool:no".to_string()
			}
		}
	}
}

fn is_valid_bare_identifier_text(value: &str) -> bool {
	let Some(&first) = value.as_bytes().first() else {
		return false;
	};
	!matches!(first, b'"' | b'-' | b'0'..=b'9')
		&& !matches!(value.to_ascii_lowercase().as_str(), "yes" | "no")
		&& !value.bytes().any(|byte| {
			matches!(
				byte,
				b' ' | b'\t' | b'\r' | b'\n' | b'=' | b'{' | b'}' | b'#'
			)
		})
}

// ------------------------------------------------------------------ classify_resolution

/// Contributor relationship between two input mods (order-independent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relationship {
	Subset,
	Redundant,
	Disjoint,
}

impl Relationship {
	pub fn as_str(self) -> &'static str {
		match self {
			Relationship::Subset => "subset",
			Relationship::Redundant => "redundant",
			Relationship::Disjoint => "disjoint",
		}
	}
}

/// How the human compatch resolved the overlap between two mods for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResVerdict {
	Identical,
	Union,
	TookBase,
	TookOverlay,
	HandEdit,
}

impl ResVerdict {
	pub fn as_str(self) -> &'static str {
		match self {
			ResVerdict::Identical => "identical",
			ResVerdict::Union => "union",
			ResVerdict::TookBase => "took_base",
			ResVerdict::TookOverlay => "took_overlay",
			ResVerdict::HandEdit => "hand_edit",
		}
	}
}

/// Human resolution classification for one overlap file (output of [`classify_resolution`]).
pub struct Resolution {
	/// Fraction of `base`'s unique lines the compatch kept; `None` if base has no unique lines.
	pub frac_base_kept: Option<f64>,
	/// Fraction of `overlay`'s unique lines the compatch kept; `None` if overlay has no unique lines.
	pub frac_overlay_kept: Option<f64>,
	/// Jaccard similarity between A and B (rounded to 2 dp).
	pub ab_jaccard: f64,
	/// Order-independent contributor relationship.
	pub relationship: Relationship,
	/// How the human compatch resolved the overlap.
	pub verdict: ResVerdict,
}

/// Faithful port of Python `classify_resolution`.
///
/// Reads `base/rel`, `overlay/rel`, `compatch/rel`; returns `None` if any file
/// is missing or unreadable.  Fractions and jaccard are rounded to 2 dp for
/// display; the `>= 0.5` threshold comparisons use the unrounded values.
pub fn classify_resolution(
	rel: &str,
	base: &Path,
	overlay: &Path,
	compatch: &Path,
) -> Option<Resolution> {
	let h = read(&compatch.join(rel))?;
	let a = read(&base.join(rel))?;
	let b = read(&overlay.join(rel))?;

	let hs: HashSet<String> = normalise(&h).into_iter().collect();
	let as_: HashSet<String> = normalise(&a).into_iter().collect();
	let bs: HashSet<String> = normalise(&b).into_iter().collect();

	let a_only: HashSet<String> = as_.difference(&bs).cloned().collect();
	let b_only: HashSet<String> = bs.difference(&as_).cloned().collect();

	let inter_len = as_.intersection(&bs).count();
	let union_len = as_.union(&bs).count();
	let jaccard = if union_len == 0 {
		1.0
	} else {
		inter_len as f64 / union_len as f64
	};

	let relationship = if a_only.is_empty() || b_only.is_empty() {
		Relationship::Subset
	} else if jaccard >= 0.5 {
		Relationship::Redundant
	} else {
		Relationship::Disjoint
	};

	// Fraction of each side's unique lines that appear in the human compatch.
	let fa = if a_only.is_empty() {
		None
	} else {
		Some(a_only.iter().filter(|l| hs.contains(l.as_str())).count() as f64 / a_only.len() as f64)
	};
	let fb = if b_only.is_empty() {
		None
	} else {
		Some(b_only.iter().filter(|l| hs.contains(l.as_str())).count() as f64 / b_only.len() as f64)
	};

	const T: f64 = 0.5;
	// keep_a = fa is None OR fa >= T  (mirrors Python exactly)
	let keep_a = fa.is_none_or(|f| f >= T);
	let keep_b = fb.is_none_or(|f| f >= T);

	let verdict = if a_only.is_empty() && b_only.is_empty() {
		ResVerdict::Identical
	} else if !a_only.is_empty() && !b_only.is_empty() {
		match (keep_a, keep_b) {
			(true, true) => ResVerdict::Union,
			(true, false) => ResVerdict::TookBase,
			(false, true) => ResVerdict::TookOverlay,
			(false, false) => ResVerdict::HandEdit,
		}
	} else if !a_only.is_empty() {
		// overlay adds nothing unique to a
		if keep_a {
			ResVerdict::TookBase
		} else {
			ResVerdict::HandEdit
		}
	} else {
		// base adds nothing unique to b
		if keep_b {
			ResVerdict::TookOverlay
		} else {
			ResVerdict::HandEdit
		}
	};

	let round2 = |v: f64| (v * 100.0).round() / 100.0;
	Some(Resolution {
		frac_base_kept: fa.map(round2),
		frac_overlay_kept: fb.map(round2),
		ab_jaccard: round2(jaccard),
		relationship,
		verdict,
	})
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod classify_tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	fn write_file(dir: &Path, rel: &str, content: &str) {
		let path = dir.join(rel);
		if let Some(p) = path.parent() {
			fs::create_dir_all(p).unwrap();
		}
		fs::write(path, content).unwrap();
	}

	fn make_dirs() -> (TempDir, TempDir, TempDir) {
		(
			tempfile::tempdir().unwrap(),
			tempfile::tempdir().unwrap(),
			tempfile::tempdir().unwrap(),
		)
	}

	#[test]
	fn cr_identical() {
		let (b, o, c) = make_dirs();
		let content = "a = 1\nb = 2\n";
		write_file(b.path(), "f.txt", content);
		write_file(o.path(), "f.txt", content);
		write_file(c.path(), "f.txt", content);
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::Identical, "verdict");
		assert_eq!(res.relationship, Relationship::Subset, "relationship");
		assert_eq!(res.ab_jaccard, 1.0, "jaccard");
		assert_eq!(res.frac_base_kept, None, "fa");
		assert_eq!(res.frac_overlay_kept, None, "fb");
	}

	#[test]
	fn cr_union() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps both unique lines
		write_file(c.path(), "f.txt", "common = 1\nx = 1\ny = 2\n");
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::Union, "verdict");
		assert_eq!(res.relationship, Relationship::Disjoint, "relationship");
		assert_eq!(res.frac_base_kept, Some(1.0), "fa");
		assert_eq!(res.frac_overlay_kept, Some(1.0), "fb");
	}

	#[test]
	fn cr_took_base() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps only base's unique line
		write_file(c.path(), "f.txt", "common = 1\nx = 1\n");
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::TookBase, "verdict");
		assert_eq!(res.frac_base_kept, Some(1.0), "fa");
		assert_eq!(res.frac_overlay_kept, Some(0.0), "fb");
	}

	#[test]
	fn cr_took_overlay() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps only overlay's unique line
		write_file(c.path(), "f.txt", "common = 1\ny = 2\n");
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::TookOverlay, "verdict");
		assert_eq!(res.frac_base_kept, Some(0.0), "fa");
		assert_eq!(res.frac_overlay_kept, Some(1.0), "fb");
	}

	#[test]
	fn cr_hand_edit() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps neither side's unique line
		write_file(c.path(), "f.txt", "common = 1\nz = 3\n");
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::HandEdit, "verdict");
		assert_eq!(res.frac_base_kept, Some(0.0), "fa");
		assert_eq!(res.frac_overlay_kept, Some(0.0), "fb");
	}

	#[test]
	fn cr_missing_file_returns_none() {
		let (b, _o, c) = make_dirs();
		write_file(b.path(), "f.txt", "a = 1\n");
		// overlay file is absent → None
		write_file(c.path(), "f.txt", "a = 1\n");
		let res = classify_resolution("f.txt", b.path(), _o.path(), c.path());
		assert!(res.is_none(), "expect None when a file is missing");
	}

	#[test]
	fn cr_subset_relationship() {
		let (b, o, c) = make_dirs();
		// overlay is a subset of base (b_only is empty)
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\n");
		write_file(c.path(), "f.txt", "common = 1\nx = 1\n");
		let res = classify_resolution("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.relationship, Relationship::Subset, "relationship");
		assert_eq!(
			res.verdict,
			ResVerdict::TookBase,
			"verdict (subset, kept base unique)"
		);
		assert_eq!(res.frac_base_kept, Some(1.0), "fa");
		assert_eq!(res.frac_overlay_kept, None, "fb (no overlay unique lines)");
	}

	#[test]
	fn ast_match_is_order_insensitive_for_non_gui_files() {
		let (foch, compatch, _) = make_dirs();
		write_file(
			foch.path(),
			"common/rebel_types/example.txt",
			"b = { y = 2 x = 1 }\na = yes\n",
		);
		write_file(
			compatch.path(),
			"common/rebel_types/example.txt",
			"a = yes\nb = { x = 1 y = 2 }\n",
		);

		assert_eq!(
			ast_match_for_path(
				"common/rebel_types/example.txt",
				&foch.path().join("common/rebel_types/example.txt"),
				&compatch.path().join("common/rebel_types/example.txt"),
			),
			Some(true)
		);
	}

	#[test]
	fn ast_match_is_order_sensitive_for_gui_files() {
		let (foch, compatch, _) = make_dirs();
		write_file(
			foch.path(),
			"interface/example.gui",
			"guiTypes = { a = yes b = yes }\n",
		);
		write_file(
			compatch.path(),
			"interface/example.gui",
			"guiTypes = { b = yes a = yes }\n",
		);

		assert_eq!(
			ast_match_for_path(
				"interface/example.gui",
				&foch.path().join("interface/example.gui"),
				&compatch.path().join("interface/example.gui"),
			),
			Some(false)
		);
	}

	#[test]
	fn ast_match_treats_quoted_identifier_text_as_equivalent() {
		let (foch, compatch, _) = make_dirs();
		write_file(foch.path(), "events/example.txt", "id = foch_event\n");
		write_file(
			compatch.path(),
			"events/example.txt",
			"id = \"foch_event\"\n",
		);

		assert_eq!(
			ast_match_for_path(
				"events/example.txt",
				&foch.path().join("events/example.txt"),
				&compatch.path().join("events/example.txt"),
			),
			Some(true)
		);
	}

	#[test]
	fn score_file_accepts_gfx_order_only_as_equivalent() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/example.gfx";
		write_file(
			mod_a.path(),
			rel,
			r#"spriteTypes = { spriteType = { name = "A" } spriteType = { name = "B" } }"#,
		);
		write_file(
			mod_b.path(),
			rel,
			r#"spriteTypes = { spriteType = { name = "A" } spriteType = { name = "B" } }"#,
		);
		write_file(
			compatch.path(),
			rel,
			r#"spriteTypes = { spriteType = { name = "A" } spriteType = { name = "B" } }"#,
		);
		write_file(
			out.path(),
			rel,
			r#"spriteTypes = { spriteType = { name = "B" } spriteType = { name = "A" } }"#,
		);

		let score = score_file(&ScoreFileRequest {
			compatch_id: "case",
			rel,
			mod_a: mod_a.path(),
			mod_b: mod_b.path(),
			compatch: compatch.path(),
			out_dir: out.path(),
			basegame_root: None,
			conflict_paths: &HashSet::new(),
			adjudications: &Adjudications::default(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::AcceptedEquivalent);
		assert_eq!(
			score.acceptance_reason.as_deref(),
			Some("gfx_order_insensitive_ast_equivalent")
		);
	}

	#[test]
	fn score_file_keeps_gui_order_only_as_divergence() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/example.gui";
		write_file(mod_a.path(), rel, "guiTypes = { a = yes b = yes }\n");
		write_file(mod_b.path(), rel, "guiTypes = { a = yes b = yes }\n");
		write_file(compatch.path(), rel, "guiTypes = { a = yes b = yes }\n");
		write_file(out.path(), rel, "guiTypes = { b = yes a = yes }\n");

		let score = score_file(&ScoreFileRequest {
			compatch_id: "case",
			rel,
			mod_a: mod_a.path(),
			mod_b: mod_b.path(),
			compatch: compatch.path(),
			out_dir: out.path(),
			basegame_root: None,
			conflict_paths: &HashSet::new(),
			adjudications: &Adjudications::default(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::DivergesAst);
		assert_eq!(score.acceptance_reason, None);
	}

	#[test]
	fn score_file_accepts_static_assignment_key_family_cross_file_equivalence() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "common/governments/00_governments.txt";
		let full_module = "monarchy = { rank = 1 }\nrepublic = { rank = 2 }\n";
		write_file(mod_a.path(), rel, full_module);
		write_file(mod_b.path(), rel, full_module);
		write_file(compatch.path(), rel, full_module);
		write_file(out.path(), rel, "monarchy = { rank = 1 }\n");
		write_file(
			out.path(),
			"common/governments/zzz_00_governments.txt",
			"republic = { rank = 2 }\n",
		);

		let score = score_file(&ScoreFileRequest {
			compatch_id: "case",
			rel,
			mod_a: mod_a.path(),
			mod_b: mod_b.path(),
			compatch: compatch.path(),
			out_dir: out.path(),
			basegame_root: None,
			conflict_paths: &HashSet::new(),
			adjudications: &Adjudications::default(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::AcceptedEquivalent);
		assert_eq!(
			score.acceptance_reason.as_deref(),
			Some("same_family_module_equivalent")
		);
	}

	#[test]
	fn score_file_keeps_gui_cross_file_difference_as_divergence() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/example.gui";
		let full_module = "guiTypes = { a = yes b = yes }\n";
		write_file(mod_a.path(), rel, full_module);
		write_file(mod_b.path(), rel, full_module);
		write_file(compatch.path(), rel, full_module);
		write_file(out.path(), rel, "guiTypes = { a = yes }\n");
		write_file(
			out.path(),
			"interface/other.gui",
			"guiTypes = { b = yes }\n",
		);

		let score = score_file(&ScoreFileRequest {
			compatch_id: "case",
			rel,
			mod_a: mod_a.path(),
			mod_b: mod_b.path(),
			compatch: compatch.path(),
			out_dir: out.path(),
			basegame_root: None,
			conflict_paths: &HashSet::new(),
			adjudications: &Adjudications::default(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::DivergesAst);
		assert_eq!(score.acceptance_reason, None);
	}

	#[test]
	fn module_family_eligibility_keeps_path_sensitive_roots_out() {
		assert!(eligible_module_family("common/governments/00_governments.txt").is_some());
		assert!(eligible_module_family("interface/example.gui").is_none());
		assert!(eligible_module_family("history/countries/FRA - France.txt").is_none());
		assert!(eligible_module_family("common/technology.txt").is_none());
	}

	#[test]
	fn score_file_applies_accepted_better_adjudication() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "common/scripted_triggers/example.txt";
		write_file(mod_a.path(), rel, "trigger = { tag = FRA }\n");
		write_file(mod_b.path(), rel, "trigger = { tag = FRA }\n");
		write_file(compatch.path(), rel, "trigger = { tag = FRA }\n");
		write_file(out.path(), rel, "trigger = { tag = ENG }\n");
		let adjudications = Adjudications::from_json(
			r#"[{
				"compatch_id": "case",
				"rel": "common/scripted_triggers/example.txt",
				"verdict": "accepted_better",
				"reason": "foch preserves the intended corrected country tag"
			}]"#,
		)
		.unwrap();

		let score = score_file(&ScoreFileRequest {
			compatch_id: "case",
			rel,
			mod_a: mod_a.path(),
			mod_b: mod_b.path(),
			compatch: compatch.path(),
			out_dir: out.path(),
			basegame_root: None,
			conflict_paths: &HashSet::new(),
			adjudications: &adjudications,
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::AcceptedBetter);
		assert_eq!(
			score.acceptance_reason.as_deref(),
			Some("foch preserves the intended corrected country tag")
		);
	}
}
