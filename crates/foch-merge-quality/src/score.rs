//! Scoring primitives for classifying how an existing generated output tree
//! compares with a human compatch — structurally and by line similarity.
//!
//! This is a faithful port of the Python harness's scoring so the verdicts are
//! identical. Scoring consumes an existing output tree; merge execution belongs
//! to the product integration layer.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use foch_core::domain::descriptor::load_descriptor;
use foch_core::domain::game::Game;
use foch_core::model::{DeferredUnitReason, DocumentFamily, MergeReport};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentFamilyPathMatcher, ContentLoadPolicy, DefinitionModulePolicy,
	GameProfile, MergeKeySource,
};
use foch_language::analyzer::definition_module::{DefinitionModuleInput, load_definition_module};
use foch_language::analyzer::documents::classify_document_family;
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, parse_clausewitz_content, parse_clausewitz_file,
};
use foch_language::analyzer::semantic_index::parse_script_file;
use regex::Regex;

/// `^key = {` at a line start — a top-level Clausewitz definition.
static TOP_KEY_RE: LazyLock<Regex> =
	LazyLock::new(|| Regex::new(r"(?m)^([A-Za-z_][\w.\-]*)\s*=\s*\{").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
/// `for <path>;` inside a conflict warning string.
static WARN_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"for ([\w./\-]+);").unwrap());

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

/// Relative paths of every supported text document in the compatch reference
/// output. Traversal is pruned at non-loadable top-level roots, and file
/// admission uses the analyzer's explicit [`DocumentFamily`] whitelist.
pub fn reference_output_files(compatch_dir: &Path) -> io::Result<Vec<String>> {
	let mut out = Vec::new();
	let loadable_roots = Game::EuropaUniversalis4
		.loadable_content_roots()
		.expect("EU4 loadable roots are defined");
	for entry in walkdir::WalkDir::new(compatch_dir)
		.into_iter()
		.filter_entry(|entry| {
			walk_entry_is_under_loadable_root(compatch_dir, entry, loadable_roots)
		}) {
		let entry = entry.map_err(io::Error::other)?;
		if !entry.file_type().is_file() {
			continue;
		}
		let relative = entry
			.path()
			.strip_prefix(compatch_dir)
			.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
		if is_supported_text_document(relative) {
			out.push(relative.to_string_lossy().replace('\\', "/"));
		}
	}
	out.sort();
	Ok(out)
}

fn walk_entry_is_under_loadable_root(
	root: &Path,
	entry: &walkdir::DirEntry,
	loadable_roots: &[&str],
) -> bool {
	if entry.depth() == 0 {
		return true;
	}
	let Ok(relative) = entry.path().strip_prefix(root) else {
		return false;
	};
	let Some(Component::Normal(top_level)) = relative.components().next() else {
		return false;
	};
	let top_level = top_level.to_string_lossy();
	loadable_roots
		.iter()
		.any(|candidate| top_level.eq_ignore_ascii_case(candidate))
}

fn is_supported_text_document(relative: &Path) -> bool {
	Game::EuropaUniversalis4.is_loadable_content_path(relative)
		&& matches!(
			classify_document_family(relative),
			Some(
				DocumentFamily::Clausewitz
					| DocumentFamily::Localisation
					| DocumentFamily::Csv
					| DocumentFamily::Json
			)
		)
}

/// Return the scorer's requested paths unchanged. The engine owns semantic
/// module expansion so production merges and corpus scoring use one closure
/// rule and one cache identity.
pub fn scoring_requested_paths(ground_truth: &[String]) -> BTreeSet<String> {
	ground_truth.iter().cloned().collect()
}

/// Collapse raw compatch paths into deterministic scoring units. Definition
/// modules contribute one unit at their policy-owned output path; ordinary
/// files remain path-scoped units.
pub fn scoring_reference_units(reference_paths: &[String]) -> Vec<String> {
	let mut units = BTreeSet::new();
	let mut module_units = BTreeMap::new();
	for rel in reference_paths {
		if !is_supported_text_document(Path::new(rel)) {
			continue;
		}
		if let Some(policy) = definition_module_policy_for_path(rel) {
			module_units
				.entry(policy.namespace_prefix)
				.or_insert(policy.output_path);
		} else {
			units.insert(rel.clone());
		}
	}
	units.extend(module_units.into_values().map(str::to_string));
	units.into_iter().collect()
}

/// Enumerate the exact regular files under one runtime layer that the scorer
/// can inspect for a scoring unit. Definition-module units expand to the full
/// policy namespace; path-scoped units remain exact. `descriptor.mod` is
/// included because replace-path semantics can change the effective view.
pub fn scoring_evidence_files(root: &Path, scoring_unit: &str) -> io::Result<Vec<String>> {
	let relative = Path::new(scoring_unit);
	if scoring_unit.is_empty()
		|| relative.components().any(|component| {
			matches!(
				component,
				Component::Prefix(_)
					| Component::RootDir
					| Component::ParentDir
					| Component::CurDir
			)
		}) {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("unsafe scoring unit path {scoring_unit:?}"),
		));
	}
	if !is_supported_text_document(relative) {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("unsupported scoring unit document {scoring_unit:?}"),
		));
	}

	let mut paths = BTreeSet::new();
	push_regular_evidence_file(root, Path::new("descriptor.mod"), &mut paths)?;
	if let Some(policy) = definition_module_policy_for_path(scoring_unit) {
		let namespace = root.join(policy.namespace_prefix);
		match fs::symlink_metadata(&namespace) {
			Ok(metadata) if metadata.file_type().is_dir() => {
				for entry in walkdir::WalkDir::new(&namespace) {
					let entry = entry.map_err(io::Error::other)?;
					if !entry.file_type().is_file() {
						continue;
					}
					let relative = entry
						.path()
						.strip_prefix(root)
						.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
					if is_supported_text_document(relative) {
						paths.insert(relative.to_string_lossy().replace('\\', "/"));
					}
				}
			}
			Ok(_) => {
				return Err(io::Error::new(
					io::ErrorKind::InvalidData,
					format!(
						"scoring module namespace is not a directory: {}",
						namespace.display()
					),
				));
			}
			Err(error) if error.kind() == io::ErrorKind::NotFound => {}
			Err(error) => return Err(error),
		}
	} else {
		push_regular_evidence_file(root, relative, &mut paths)?;
	}
	Ok(paths.into_iter().collect())
}

/// Whether one scorer-visible relative path can belong to the requested unit.
/// Path-scoped units admit only their exact path; definition-module units admit
/// only files inside the policy-owned namespace. The descriptor is shared by
/// every unit because it controls replace-path visibility.
pub fn scoring_evidence_path_belongs_to_unit(scoring_unit: &str, relative_path: &str) -> bool {
	let scoring_path = Path::new(scoring_unit);
	let relative_path = Path::new(relative_path);
	if scoring_unit.is_empty()
		|| !is_supported_text_document(scoring_path)
		|| relative_path.is_absolute()
		|| relative_path.components().any(|component| {
			matches!(
				component,
				Component::Prefix(_)
					| Component::RootDir
					| Component::ParentDir
					| Component::CurDir
			)
		}) {
		return false;
	}
	if relative_path == Path::new("descriptor.mod") {
		return true;
	}
	if !is_supported_text_document(relative_path) {
		return false;
	}
	definition_module_policy_for_path(scoring_unit).map_or_else(
		|| relative_path == scoring_path,
		|policy| relative_path.starts_with(policy.namespace_prefix),
	)
}

fn push_regular_evidence_file(
	root: &Path,
	relative: &Path,
	paths: &mut BTreeSet<String>,
) -> io::Result<()> {
	let path = root.join(relative);
	match fs::symlink_metadata(&path) {
		Ok(metadata) if metadata.file_type().is_file() => {
			paths.insert(relative.to_string_lossy().replace('\\', "/"));
			Ok(())
		}
		Ok(_) if path.is_file() => Err(io::Error::new(
			io::ErrorKind::InvalidData,
			format!(
				"scoring evidence must be a regular file: {}",
				path.display()
			),
		)),
		Ok(_) => Ok(()),
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
		Err(error) => Err(error),
	}
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

/// Paths foch deferred because they require a genuine user or policy choice.
/// Unsupported inputs and engine failures are deliberately not scored as
/// human merge conflicts.
pub fn conflict_rel_paths(report: &MergeReport) -> HashSet<String> {
	let mut out = HashSet::new();
	for c in &report.conflict_resolutions {
		if c.deferred_reason == DeferredUnitReason::NeedsUserChoice && !c.path.is_empty() {
			out.insert(c.path.clone());
		}
	}
	// Pre-structured reports exposed only warning text. Keep that fallback for
	// reports with no typed entries, but never let a warning overwrite a typed
	// unsupported-input or engine-failure classification.
	if report.conflict_resolutions.is_empty() {
		for w in &report.warnings {
			if let Some(m) = WARN_PATH_RE.captures(w) {
				out.insert(m[1].to_string());
			}
		}
	}
	out
}

/// Classification of foch's output for one path- or module-scoped scoring unit.
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
	records: HashMap<(String, String, String, String), AcceptedAdjudication>,
}

impl Adjudications {
	pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
		let records: Vec<AcceptedAdjudicationRecord> = serde_json::from_str(text)?;
		let records = records
			.into_iter()
			.map(|record| {
				(
					(
						record.compatch_id,
						record.rel,
						record.snapshot_id,
						record.scoring_unit_id,
					),
					AcceptedAdjudication {
						verdict: record.verdict,
						reason: record.reason,
						candidate_semantic_content_id: record.candidate_semantic_content_id,
						human_semantic_content_id: record.human_semantic_content_id,
						required_atoms: record.required_atoms,
						forbidden_atoms: record.forbidden_atoms,
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

	pub fn accepted_better_reason(
		&self,
		binding: &AdjudicationBinding<'_>,
		candidate: &AstFile,
		human: &AstFile,
	) -> Option<String> {
		let adjudication = self.records.get(&(
			binding.compatch_id.to_string(),
			binding.relative_path.to_string(),
			binding.snapshot_id.to_string(),
			binding.scoring_unit_id.to_string(),
		))?;
		let candidate_atoms = semantic_atom_bag_statements(&candidate.statements);
		let human_atoms = semantic_atom_bag_statements(&human.statements);
		if semantic_atom_bag_content_id(&candidate_atoms)
			!= adjudication.candidate_semantic_content_id
			|| semantic_atom_bag_content_id(&human_atoms) != adjudication.human_semantic_content_id
			|| adjudication
				.required_atoms
				.iter()
				.any(|(atom, count)| candidate_atoms.get(atom).copied().unwrap_or(0) < *count)
			|| adjudication
				.forbidden_atoms
				.iter()
				.any(|atom| candidate_atoms.contains_key(atom))
		{
			return None;
		}
		match adjudication.verdict {
			AcceptedAdjudicationVerdict::AcceptedBetter => Some(adjudication.reason.clone()),
		}
	}
}

#[derive(Clone, Copy, Debug)]
pub struct AdjudicationBinding<'a> {
	pub compatch_id: &'a str,
	pub relative_path: &'a str,
	pub snapshot_id: &'a str,
	pub scoring_unit_id: &'a str,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct AcceptedAdjudicationRecord {
	compatch_id: String,
	rel: String,
	snapshot_id: String,
	scoring_unit_id: String,
	verdict: AcceptedAdjudicationVerdict,
	reason: String,
	candidate_semantic_content_id: String,
	human_semantic_content_id: String,
	#[serde(default)]
	required_atoms: BTreeMap<String, usize>,
	#[serde(default)]
	forbidden_atoms: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct AcceptedAdjudication {
	verdict: AcceptedAdjudicationVerdict,
	reason: String,
	candidate_semantic_content_id: String,
	human_semantic_content_id: String,
	required_atoms: BTreeMap<String, usize>,
	forbidden_atoms: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AcceptedAdjudicationVerdict {
	AcceptedBetter,
}

#[derive(Clone, Debug)]
pub struct FileScore {
	pub rel: String,
	pub source_mod_ids: Vec<String>,
	pub source_count: usize,
	pub multi_source: bool,
	pub foch_emitted: bool,
	pub foch_conflict: bool,
	pub similarity: Option<f64>,
	pub keys_match: Option<bool>,
	pub ast_match: Option<bool>,
	pub dropped_keys: Vec<String>,
	pub verdict: Verdict,
	pub acceptance_reason: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceMod<'a> {
	pub id: &'a str,
	pub root: &'a Path,
}

pub struct ScoreFileRequest<'a> {
	pub rel: &'a str,
	pub source_mods: &'a [SourceMod<'a>],
	pub compatch: &'a Path,
	pub out_dir: &'a Path,
	pub conflict_paths: &'a HashSet<String>,
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
	fn new(bytes: &[u8]) -> Self {
		Self {
			text: foch_core::decode_paradox_bytes(bytes).into_owned(),
			normalized: None,
			keys: None,
			canonical: HashMap::new(),
		}
	}
}

type CanonicalModuleMap = BTreeMap<String, CanonicalStatement>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ModuleRootCacheIdentity {
	path: PathBuf,
	content_hash: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ModuleViewCacheKey {
	roots: Vec<ModuleRootCacheIdentity>,
	family_prefix: String,
	policy_version: u32,
}

#[derive(Default)]
pub struct ScoreCache {
	path_content: HashMap<PathBuf, Option<ContentKey>>,
	content_entries: HashMap<ContentKey, ContentEntry>,
	module_views: HashMap<ModuleViewCacheKey, Option<CanonicalModuleMap>>,
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
					self.content_entries
						.entry(key.clone())
						.or_insert_with(|| ContentEntry::new(&bytes));
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
			let parsed = parse_clausewitz_content(
				path.to_path_buf(),
				&self
					.content_entries
					.get(&content)
					.expect("content entry inserted")
					.text,
			);
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

	fn module_view(&mut self, roots: &[&Path], family_prefix: &str) -> Option<CanonicalModuleMap> {
		self.module_view_with_post_build(roots, family_prefix, || {})
	}

	fn module_view_with_post_build(
		&mut self,
		roots: &[&Path],
		family_prefix: &str,
		post_build: impl FnOnce(),
	) -> Option<CanonicalModuleMap> {
		let key = module_view_cache_key(roots, family_prefix)?;
		if !self.module_views.contains_key(&key) {
			let root_paths = key
				.roots
				.iter()
				.map(|root| root.path.clone())
				.collect::<Vec<_>>();
			let view = canonical_layered_module_view_uncached(&root_paths, family_prefix);
			post_build();
			if module_view_cache_key(roots, family_prefix).as_ref() != Some(&key) {
				return None;
			}
			self.module_views.insert(key.clone(), view);
		}
		self.module_views
			.get(&key)
			.expect("module view inserted")
			.clone()
	}
}

/// Classify foch's merged output for one scoring unit against the compatch.
pub fn score_file(request: &ScoreFileRequest<'_>) -> FileScore {
	score_file_with_basegame(request, None)
}

/// Classify one scoring unit with an optional base-game runtime layer.
pub fn score_file_with_basegame(
	request: &ScoreFileRequest<'_>,
	basegame_root: Option<&Path>,
) -> FileScore {
	let mut cache = ScoreCache::new();
	score_file_with_cache_and_basegame(request, &mut cache, basegame_root)
}

/// Classify one scoring unit, reusing parsed/text artifacts across files.
pub fn score_file_with_cache(request: &ScoreFileRequest<'_>, cache: &mut ScoreCache) -> FileScore {
	score_file_with_cache_and_basegame(request, cache, None)
}

/// Classify one scoring unit with a reusable cache and optional base-game
/// runtime layer.
pub fn score_file_with_cache_and_basegame(
	request: &ScoreFileRequest<'_>,
	cache: &mut ScoreCache,
	basegame_root: Option<&Path>,
) -> FileScore {
	if let Some(policy) = definition_module_policy_for_path(request.rel) {
		return score_definition_module(request, cache, policy, basegame_root);
	}

	let rel = request.rel;
	let compatch_path = request.compatch.join(rel);
	let source_mod_ids = request
		.source_mods
		.iter()
		.filter(|source| source.root.join(rel).is_file())
		.map(|source| source.id.to_string())
		.collect::<Vec<_>>();
	let source_count = source_mod_ids.len();
	let multi_source = source_count >= 2;
	let emitted_path = request.out_dir.join(rel);
	let foch_emitted = emitted_path.is_file();
	let runtime_path = effective_runtime_file(request, basegame_root);
	let foch_conflict = request.conflict_paths.contains(rel);

	let mut sim = None;
	let mut keys_match = None;
	let mut ast_match = None;
	let mut policy_equivalent = false;
	let mut dropped: Vec<String> = Vec::new();
	if let Some(foch_path) = &runtime_path {
		let fk = cache.top_level_keys(foch_path);
		let ck = cache.top_level_keys(&compatch_path);
		keys_match = Some(fk == ck);
		ast_match = ast_match_for_path_cached(cache, rel, foch_path, &compatch_path);
		if ast_match == Some(true) {
			sim = cache.rounded_similarity(foch_path, &compatch_path);
		}
		policy_equivalent = ast_match == Some(false)
			&& accepted_equivalent_for_path(cache, rel, foch_path, &compatch_path);
	}
	if let Some(foch_path) = &runtime_path {
		let mut source_keys = HashSet::new();
		for source in request.source_mods {
			source_keys.extend(cache.top_level_keys(&source.root.join(rel)));
		}
		let foch_keys = cache.top_level_keys(foch_path);
		dropped = source_keys.difference(&foch_keys).cloned().collect();
		dropped.sort();
	}

	let (verdict, acceptance_reason) = if foch_conflict {
		(Verdict::ConflictWithheld, None)
	} else if ast_match == Some(true) && sim.is_some_and(|s| s >= 0.92) {
		(Verdict::MatchesHuman, None)
	} else if ast_match == Some(true) {
		(Verdict::MatchesAst, None)
	} else if policy_equivalent {
		(
			Verdict::AcceptedEquivalent,
			Some("gfx_order_insensitive_ast_equivalent".to_string()),
		)
	} else if runtime_path.is_none() {
		(Verdict::NotEmitted, None)
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
		source_mod_ids,
		source_count,
		multi_source,
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

fn effective_runtime_file(
	request: &ScoreFileRequest<'_>,
	basegame_root: Option<&Path>,
) -> Option<PathBuf> {
	let output = request.out_dir.join(request.rel);
	if output.is_file() {
		return Some(output);
	}
	if layer_replaces_module(request.out_dir, request.rel) == Some(true) {
		return None;
	}

	for source in request.source_mods.iter().rev() {
		let candidate = source.root.join(request.rel);
		if candidate.is_file() {
			return Some(candidate);
		}
		if layer_replaces_module(source.root, request.rel) == Some(true) {
			return None;
		}
	}

	basegame_root
		.map(|root| root.join(request.rel))
		.filter(|path| path.is_file())
}

fn score_definition_module(
	request: &ScoreFileRequest<'_>,
	cache: &mut ScoreCache,
	policy: DefinitionModulePolicy,
	basegame_root: Option<&Path>,
) -> FileScore {
	let rel = policy.output_path;
	let prefix = policy.namespace_prefix;
	let mut human_roots = Vec::with_capacity(request.source_mods.len() + 2);
	let mut foch_roots = Vec::with_capacity(request.source_mods.len() + 2);
	if let Some(root) = basegame_root {
		human_roots.push(root);
		foch_roots.push(root);
	}
	for source in request.source_mods {
		human_roots.push(source.root);
		foch_roots.push(source.root);
	}
	human_roots.push(request.compatch);
	foch_roots.push(request.out_dir);
	let human = cache.module_view(&human_roots, prefix);
	let foch = cache.module_view(&foch_roots, prefix);
	let human_keys = human
		.as_ref()
		.map(|view| view.keys().cloned().collect::<BTreeSet<_>>())
		.unwrap_or_default();
	let source_mod_ids = source_mod_ids_for_module(cache, request.source_mods, prefix, &human_keys);
	let source_count = source_mod_ids.len();
	let foch_conflict = request.conflict_paths.contains(rel);
	let mut keys_match = None;
	let mut ast_match = None;
	let mut dropped_keys = Vec::new();
	let mut has_extra_keys = false;
	if let (Some(human), Some(foch)) = (&human, &foch) {
		dropped_keys = human
			.keys()
			.filter(|key| !foch.contains_key(*key))
			.cloned()
			.collect();
		has_extra_keys = foch.keys().any(|key| !human.contains_key(key));
		keys_match = Some(dropped_keys.is_empty() && !has_extra_keys);
		ast_match = Some(human == foch);
	}
	let (verdict, acceptance_reason) = if foch_conflict {
		(Verdict::ConflictWithheld, None)
	} else {
		match (&human, &foch) {
			(Some(human), Some(foch)) if human == foch => (
				Verdict::AcceptedEquivalent,
				Some("same_family_module_equivalent".to_string()),
			),
			(Some(_), Some(_)) if !dropped_keys.is_empty() => (Verdict::DropsContent, None),
			(Some(_), Some(_)) if has_extra_keys => (Verdict::DivergesStructure, None),
			(Some(_), Some(_)) => (Verdict::DivergesAst, None),
			(None, _) | (_, None) => (Verdict::DivergesStructure, None),
		}
	};

	FileScore {
		rel: rel.to_string(),
		source_mod_ids,
		source_count,
		multi_source: source_count >= 2,
		foch_emitted: request.out_dir.join(rel).is_file(),
		foch_conflict,
		similarity: None,
		keys_match,
		ast_match,
		dropped_keys,
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

#[derive(Clone, Debug)]
struct ModuleTarget {
	prefix: &'static str,
	keys: BTreeSet<String>,
}

fn module_target_context(
	cache: &mut ScoreCache,
	rel: &str,
	target_roots: &[&Path],
) -> Option<ModuleTarget> {
	let descriptor = eligible_module_family(rel)?;
	let prefix = family_prefix(descriptor)?;
	let keys: BTreeSet<String> = cache
		.module_view(target_roots, prefix)?
		.into_keys()
		.collect();
	if keys.is_empty() {
		return None;
	}
	Some(ModuleTarget { prefix, keys })
}

fn source_mod_ids_for_module(
	cache: &mut ScoreCache,
	sources: &[SourceMod<'_>],
	prefix: &str,
	human_keys: &BTreeSet<String>,
) -> Vec<String> {
	sources
		.iter()
		.filter(|source| {
			cache
				.module_view(&[source.root], prefix)
				.is_some_and(|view| human_keys.iter().any(|key| view.contains_key(key)))
		})
		.map(|source| source.id.to_string())
		.collect()
}

fn eligible_module_family(rel: &str) -> Option<&'static ContentFamilyDescriptor> {
	if is_path_sensitive_for_module_scoring(rel) {
		return None;
	}
	let descriptor = eu4_profile().classify_content_family(Path::new(rel))?;
	if !matches!(descriptor.matcher, ContentFamilyPathMatcher::Prefix(_)) {
		return None;
	}
	if !matches!(
		descriptor.merge_key_source,
		Some(
			MergeKeySource::AssignmentKey
				| MergeKeySource::FieldValue(_)
				| MergeKeySource::ChildFieldValue { .. }
		)
	) {
		return None;
	}
	if !matches!(
		descriptor.load_policy,
		ContentLoadPolicy::DefinitionModule(_)
	) {
		return None;
	}
	Some(descriptor)
}

fn family_prefix(descriptor: &ContentFamilyDescriptor) -> Option<&'static str> {
	match descriptor.load_policy {
		ContentLoadPolicy::DefinitionModule(policy) => Some(policy.namespace_prefix),
		ContentLoadPolicy::PerPath => None,
	}
}

pub(crate) fn definition_module_policy_for_path(rel: &str) -> Option<DefinitionModulePolicy> {
	let descriptor = eligible_module_family(rel)?;
	match descriptor.load_policy {
		ContentLoadPolicy::DefinitionModule(policy) => Some(policy),
		ContentLoadPolicy::PerPath => None,
	}
}

fn definition_module_policy_for_prefix(prefix: &str) -> Option<DefinitionModulePolicy> {
	let probe = format!("{}/__foch_module__.txt", prefix.trim_end_matches('/'));
	let descriptor = eu4_profile().classify_content_family(Path::new(&probe))?;
	match descriptor.load_policy {
		ContentLoadPolicy::DefinitionModule(policy) if policy.namespace_prefix == prefix => {
			Some(policy)
		}
		ContentLoadPolicy::DefinitionModule(_) | ContentLoadPolicy::PerPath => None,
	}
}

fn canonical_layered_module_view_uncached(
	roots: &[PathBuf],
	family_prefix: &str,
) -> Option<CanonicalModuleMap> {
	let policy = definition_module_policy_for_prefix(family_prefix)?;
	let merge_key_source = definition_module_merge_key_for_prefix(family_prefix)?;
	let mut visible_files = BTreeMap::<String, (usize, PathBuf, PathBuf)>::new();
	for (layer_ordinal, root) in roots.iter().enumerate() {
		if layer_replaces_module(root, family_prefix)? {
			visible_files.clear();
		}
		for (relative, path) in module_files(root, family_prefix)? {
			visible_files.insert(relative, (layer_ordinal, root.clone(), path));
		}
	}

	let mut parsed_files = Vec::with_capacity(visible_files.len());
	for (relative, (layer_ordinal, root, path)) in visible_files {
		let parsed = parse_script_file("__score__", &root, &path)?;
		parsed_files.push((layer_ordinal, relative, parsed));
	}
	let inputs = parsed_files
		.iter()
		.map(|(layer_ordinal, path, file)| {
			DefinitionModuleInput::new(Path::new(path), file).with_layer_ordinal(*layer_ordinal)
		})
		.collect::<Vec<_>>();
	let module = load_definition_module(&inputs, policy).ok()?;
	Some(
		module
			.ast
			.statements
			.iter()
			.filter_map(|statement| canonical_module_assignment(statement, merge_key_source))
			.collect(),
	)
}

fn definition_module_merge_key_for_prefix(prefix: &str) -> Option<MergeKeySource> {
	let probe = format!("{}/__foch_module__.txt", prefix.trim_end_matches('/'));
	let descriptor = eligible_module_family(&probe)?;
	descriptor.merge_key_source
}

fn layer_replaces_module(root: &Path, family_prefix: &str) -> Option<bool> {
	let descriptor_path = root.join("descriptor.mod");
	if !descriptor_path.is_file() {
		return Some(false);
	}
	let descriptor = load_descriptor(&descriptor_path).ok()?;
	Some(
		descriptor
			.replace_path
			.iter()
			.any(|replace_path| replace_path_covers_prefix(replace_path, family_prefix)),
	)
}

fn replace_path_covers_prefix(replace_path: &str, family_prefix: &str) -> bool {
	let normalized = replace_path.trim().replace('\\', "/");
	let replace_path = normalized.trim_matches('/');
	let family_prefix = family_prefix.trim_matches('/');
	!replace_path.is_empty()
		&& (replace_path == family_prefix
			|| family_prefix
				.strip_prefix(replace_path)
				.is_some_and(|suffix| suffix.starts_with('/')))
}

#[cfg(test)]
fn canonical_module_view_uncached(root: &Path, family_prefix: &str) -> Option<CanonicalModuleMap> {
	canonical_layered_module_view_uncached(&[root.to_path_buf()], family_prefix)
}

fn module_view_cache_key(roots: &[&Path], family_prefix: &str) -> Option<ModuleViewCacheKey> {
	let policy = definition_module_policy_for_prefix(family_prefix)?;
	let roots = roots
		.iter()
		.map(|root| {
			Some(ModuleRootCacheIdentity {
				path: root.to_path_buf(),
				content_hash: module_root_content_hash(root, family_prefix)?,
			})
		})
		.collect::<Option<Vec<_>>>()?;
	Some(ModuleViewCacheKey {
		roots,
		family_prefix: family_prefix.to_string(),
		policy_version: policy.policy_version,
	})
}

fn module_root_content_hash(root: &Path, family_prefix: &str) -> Option<String> {
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-merge-quality-definition-module-root-v1");
	hash_module_component(&mut hasher, family_prefix.as_bytes());
	match fs::read(root.join("descriptor.mod")) {
		Ok(bytes) => {
			hasher.update(&[1]);
			hash_module_component(&mut hasher, &bytes);
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			hasher.update(&[0]);
		}
		Err(_) => return None,
	}
	for (relative, path) in module_files(root, family_prefix)? {
		hash_module_component(&mut hasher, relative.as_bytes());
		hash_module_component(&mut hasher, &fs::read(path).ok()?);
	}
	Some(hasher.finalize().to_hex().to_string())
}

fn module_files(root: &Path, family_prefix: &str) -> Option<Vec<(String, PathBuf)>> {
	let family_dir = root.join(family_prefix);
	match fs::metadata(&family_dir) {
		Ok(metadata) if metadata.is_dir() => {}
		Ok(_) => return Some(Vec::new()),
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(Vec::new()),
		Err(_) => return None,
	}
	collect_module_files(root, walkdir::WalkDir::new(&family_dir))
}

fn collect_module_files(
	root: &Path,
	entries: impl IntoIterator<Item = Result<walkdir::DirEntry, walkdir::Error>>,
) -> Option<Vec<(String, PathBuf)>> {
	let mut files = Vec::new();
	for entry in entries {
		let entry = entry.ok()?;
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.into_path();
		let relative = path.strip_prefix(root).ok()?;
		if !is_supported_text_document(relative) {
			continue;
		}
		files.push((relative_module_path(root, &path), path));
	}
	files.sort_by(|left, right| left.0.cmp(&right.0));
	Some(files)
}

fn hash_module_component(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

fn relative_module_path(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
}

fn canonical_module_assignment(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Option<(String, CanonicalStatement)> {
	let AstStatement::Assignment { key, value, .. } = statement else {
		return None;
	};
	let merge_key = match merge_key_source {
		MergeKeySource::AssignmentKey => key.clone(),
		MergeKeySource::FieldValue(field) => scalar_field_value(value, field)?,
		MergeKeySource::ChildFieldValue {
			child_key_field,
			child_types,
		} => {
			if child_types.is_empty() || child_types.contains(&key.as_str()) {
				scalar_field_value(value, child_key_field)
					.map(|field_value| format!("{key}:{field_value}"))
					.unwrap_or_else(|| key.clone())
			} else {
				key.clone()
			}
		}
		MergeKeySource::ContainerChildKey
		| MergeKeySource::ContainerChildFieldValue { .. }
		| MergeKeySource::LeafPath => return None,
	};
	Some((
		merge_key,
		CanonicalStatement::Assignment {
			key: key.clone(),
			value: canonical_value(value, AstOrderingPolicy::OrderInsensitive),
		},
	))
}

fn scalar_field_value(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} if key == field => Some(value.as_text()),
		_ => None,
	})
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

fn canonical_atoms_for_keys(
	view: &BTreeMap<String, CanonicalStatement>,
	keys: &BTreeSet<String>,
) -> AtomBag {
	let mut atoms = AtomBag::new();
	for key in keys {
		if let Some(statement) = view.get(key) {
			flatten_canonical_statement(statement, &[], &mut atoms);
		}
	}
	atoms
}

fn flatten_canonical_statement(
	statement: &CanonicalStatement,
	prefix: &[String],
	atoms: &mut AtomBag,
) {
	match statement {
		CanonicalStatement::Assignment { key, value } => {
			let mut path = prefix.to_vec();
			path.push(format!("assignment:{key}"));
			flatten_canonical_value(value, &path, atoms);
		}
		CanonicalStatement::Item(value) => {
			let mut path = prefix.to_vec();
			path.push("item".to_string());
			flatten_canonical_value(value, &path, atoms);
		}
	}
}

fn flatten_canonical_value(value: &CanonicalValue, path: &[String], atoms: &mut AtomBag) {
	match value {
		CanonicalValue::Scalar(value) => {
			*atoms
				.entry(format!("{}={value}", path.join("/")))
				.or_default() += 1;
		}
		CanonicalValue::Block(statements) if statements.is_empty() => {
			*atoms
				.entry(format!("{}=block:{{}}", path.join("/")))
				.or_default() += 1;
		}
		CanonicalValue::Block(statements) => {
			for statement in statements {
				flatten_canonical_statement(statement, path, atoms);
			}
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResVerdict {
	Identical,
	Union,
	TookBase,
	TookOverlay,
	PartialUnion,
	HandEdit,
}

impl ResVerdict {
	pub fn as_str(self) -> &'static str {
		match self {
			ResVerdict::Identical => "identical",
			ResVerdict::Union => "union",
			ResVerdict::TookBase => "took_base",
			ResVerdict::TookOverlay => "took_overlay",
			ResVerdict::PartialUnion => "partial_union",
			ResVerdict::HandEdit => "hand_edit",
		}
	}
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContributorRetention {
	pub source_id: String,
	pub unique_atoms: usize,
	pub retained_unique_atoms: usize,
	pub fraction_kept: Option<f64>,
}

/// Human resolution classification for one overlap file (output of [`classify_resolution`]).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Resolution {
	pub contributors: Vec<ContributorRetention>,
	/// Generalized multiset Jaccard across all source mods, rounded to 2 dp.
	pub source_jaccard: f64,
	/// Human atoms not present in any source after base-game subtraction.
	pub human_only_atoms: usize,
	/// Base-game atoms removed from the human target before classification.
	pub basegame_atoms_subtracted: usize,
	/// Order-independent contributor relationship.
	pub relationship: Relationship,
	/// How the human compatch resolved the overlap.
	pub verdict: ResVerdict,
}

type AtomBag = BTreeMap<String, usize>;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SemanticAtomDiff {
	pub left_atoms: usize,
	pub right_atoms: usize,
	pub shared_atoms: usize,
	pub left_only: BTreeMap<String, usize>,
	pub right_only: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ReviewSemanticLayer {
	pub semantic_content_id: String,
	pub atoms: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ReviewSemanticSource {
	pub source_id: String,
	pub layer: ReviewSemanticLayer,
	pub vs_base: SemanticAtomDiff,
	pub candidate_vs_source: SemanticAtomDiff,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ReviewSemanticEvidence {
	pub base: ReviewSemanticLayer,
	pub sources: Vec<ReviewSemanticSource>,
	pub human: ReviewSemanticLayer,
	pub candidate: ReviewSemanticLayer,
	pub human_vs_base: SemanticAtomDiff,
	pub candidate_vs_base: SemanticAtomDiff,
	pub candidate_vs_human: SemanticAtomDiff,
}

/// Compare two files using the scorer's format-aware semantic atom model.
pub fn semantic_atom_diff(
	rel: &str,
	left: &Path,
	right: &Path,
	ignore_order: bool,
) -> Option<SemanticAtomDiff> {
	let ordering = ignore_order.then_some(AstOrderingPolicy::OrderInsensitive);
	let left = semantic_atoms_for_path_with_ordering(rel, left, ordering)?;
	let right = semantic_atoms_for_path_with_ordering(rel, right, ordering)?;
	Some(semantic_atom_bag_diff(&left, &right))
}

/// Build the scorer's complete base/source/human/candidate semantic evidence
/// without changing the verdict policy.
pub fn review_semantic_evidence(
	request: &ScoreFileRequest<'_>,
	basegame_root: Option<&Path>,
) -> Option<ReviewSemanticEvidence> {
	let mut cache = ScoreCache::new();
	review_semantic_evidence_with_cache(request, &mut cache, basegame_root)
}

/// Build review evidence while reusing parsed content and module views across
/// scoring units.
pub fn review_semantic_evidence_with_cache(
	request: &ScoreFileRequest<'_>,
	cache: &mut ScoreCache,
	basegame_root: Option<&Path>,
) -> Option<ReviewSemanticEvidence> {
	let mut base_roots = Vec::new();
	if let Some(root) = basegame_root {
		base_roots.push(root);
	}
	let base = semantic_atoms_for_runtime_layers(cache, request.rel, &base_roots)?;

	let mut source_layers = Vec::with_capacity(request.source_mods.len());
	for source in request.source_mods {
		let mut roots = base_roots.clone();
		roots.push(source.root);
		let layer = semantic_atoms_for_runtime_layers(cache, request.rel, &roots)?;
		source_layers.push((source.id.to_string(), layer));
	}

	let mut human_roots = base_roots.clone();
	human_roots.extend(request.source_mods.iter().map(|source| source.root));
	human_roots.push(request.compatch);
	let human = semantic_atoms_for_runtime_layers(cache, request.rel, &human_roots)?;

	let mut candidate_roots = base_roots;
	candidate_roots.extend(request.source_mods.iter().map(|source| source.root));
	candidate_roots.push(request.out_dir);
	let candidate = semantic_atoms_for_runtime_layers(cache, request.rel, &candidate_roots)?;
	let sources = source_layers
		.into_iter()
		.map(|(source_id, layer)| ReviewSemanticSource {
			source_id,
			layer: review_semantic_layer(&layer),
			vs_base: semantic_atom_bag_diff(&layer, &base),
			candidate_vs_source: semantic_atom_bag_diff(&candidate, &layer),
		})
		.collect();

	Some(ReviewSemanticEvidence {
		base: review_semantic_layer(&base),
		sources,
		human: review_semantic_layer(&human),
		candidate: review_semantic_layer(&candidate),
		human_vs_base: semantic_atom_bag_diff(&human, &base),
		candidate_vs_base: semantic_atom_bag_diff(&candidate, &base),
		candidate_vs_human: semantic_atom_bag_diff(&candidate, &human),
	})
}

fn review_semantic_layer(atoms: &AtomBag) -> ReviewSemanticLayer {
	ReviewSemanticLayer {
		semantic_content_id: semantic_atom_bag_content_id(atoms),
		atoms: bag_size(atoms),
	}
}

fn semantic_atoms_for_runtime_layers(
	cache: &mut ScoreCache,
	rel: &str,
	roots: &[&Path],
) -> Option<AtomBag> {
	if let Some(descriptor) = eligible_module_family(rel) {
		let prefix = family_prefix(descriptor)?;
		let view = cache.module_view(roots, prefix)?;
		let keys = view.keys().cloned().collect::<BTreeSet<_>>();
		return Some(canonical_atoms_for_keys(&view, &keys));
	}

	for root in roots.iter().rev() {
		let path = root.join(rel);
		if path.is_file() {
			return semantic_atoms_for_path(rel, &path);
		}
		if layer_replaces_module(root, rel) == Some(true) {
			return Some(AtomBag::new());
		}
	}
	Some(AtomBag::new())
}

pub(crate) fn semantic_atom_diff_ast(left: &AstFile, right: &AstFile) -> SemanticAtomDiff {
	semantic_atom_diff_statements(&left.statements, &right.statements)
}

pub(crate) fn semantic_atom_diff_statements(
	left: &[AstStatement],
	right: &[AstStatement],
) -> SemanticAtomDiff {
	let left_atoms = semantic_atom_bag_statements(left);
	let right_atoms = semantic_atom_bag_statements(right);
	semantic_atom_bag_diff(&left_atoms, &right_atoms)
}

pub(crate) fn semantic_ast_content_id(ast: &AstFile) -> String {
	let atoms = semantic_atom_bag_statements(&ast.statements);
	semantic_atom_bag_content_id(&atoms)
}

fn semantic_atom_bag_content_id(atoms: &AtomBag) -> String {
	let encoded = serde_json::to_vec(&atoms).expect("semantic atom bag serializes");
	blake3::hash(&encoded).to_hex().to_string()
}

fn semantic_atom_bag_statements(statements: &[AstStatement]) -> AtomBag {
	let mut atoms = AtomBag::new();
	flatten_semantic_statements(
		statements,
		AstOrderingPolicy::OrderInsensitive,
		&[],
		&mut atoms,
	);
	atoms
}

fn semantic_atom_bag_diff(left: &AtomBag, right: &AtomBag) -> SemanticAtomDiff {
	let left_atoms = bag_size(left);
	let right_atoms = bag_size(right);
	let shared_atoms = intersection_bag_size(left, right);
	let left_only = subtract_bag(left, right).0;
	let right_only = subtract_bag(right, left).0;
	SemanticAtomDiff {
		left_atoms,
		right_atoms,
		shared_atoms,
		left_only,
		right_only,
	}
}

/// Classify how the human compatch resolved every source that contributes a
/// file. Parseable Clausewitz files are compared as AST-derived semantic atoms;
/// other formats use normalized records. Base-game atoms are subtracted from
/// every source and the human target before contributor retention is measured.
pub fn classify_resolution(
	rel: &str,
	sources: &[SourceMod<'_>],
	compatch: &Path,
	basegame_root: Option<&Path>,
) -> Option<Resolution> {
	let mut module_cache = ScoreCache::new();
	let mut human_roots = Vec::with_capacity(sources.len() + 2);
	if let Some(root) = basegame_root {
		human_roots.push(root);
	}
	human_roots.extend(sources.iter().map(|source| source.root));
	human_roots.push(compatch);
	let module_target = module_target_context(&mut module_cache, rel, &human_roots);
	let (human_original, basegame, source_originals) = if let Some(target) = &module_target {
		let mut resolution_keys = target.keys.clone();
		for source in sources {
			if let Some(view) = module_cache.module_view(&[source.root], target.prefix) {
				resolution_keys.extend(view.into_keys());
			}
		}
		let human_view = module_cache.module_view(&human_roots, target.prefix)?;
		let human = canonical_atoms_for_keys(&human_view, &resolution_keys);
		let basegame = basegame_root
			.and_then(|root| module_cache.module_view(&[root], target.prefix))
			.map(|view| canonical_atoms_for_keys(&view, &resolution_keys))
			.unwrap_or_default();
		let mut source_originals = Vec::new();
		for source in sources {
			let Some(view) = module_cache.module_view(&[source.root], target.prefix) else {
				continue;
			};
			if !resolution_keys.iter().any(|key| view.contains_key(key)) {
				continue;
			}
			source_originals.push((source, canonical_atoms_for_keys(&view, &resolution_keys)));
		}
		(human, basegame, source_originals)
	} else {
		let human = semantic_atoms_for_path(rel, &compatch.join(rel))?;
		let basegame = basegame_root
			.map(|root| basegame_atoms_for_path(rel, root))
			.unwrap_or_default();
		let source_originals = sources
			.iter()
			.filter_map(|source| {
				semantic_atoms_for_path(rel, &source.root.join(rel)).map(|atoms| (source, atoms))
			})
			.collect();
		(human, basegame, source_originals)
	};
	let (human, basegame_atoms_subtracted) = subtract_bag(&human_original, &basegame);
	let source_bags: Vec<(&SourceMod<'_>, AtomBag)> = source_originals
		.into_iter()
		.map(|(source, atoms)| (source, subtract_bag(&atoms, &basegame).0))
		.collect();
	if source_bags.len() < 2 {
		return None;
	}

	let source_union = union_bags(source_bags.iter().map(|(_, atoms)| atoms));
	let source_intersection = intersection_size(source_bags.iter().map(|(_, atoms)| atoms));
	let union_size = bag_size(&source_union);
	let jaccard = if union_size == 0 {
		1.0
	} else {
		source_intersection as f64 / union_size as f64
	};

	let unique_bags: Vec<AtomBag> = source_bags
		.iter()
		.enumerate()
		.map(|(index, (_, atoms))| {
			let others = union_bags(
				source_bags
					.iter()
					.enumerate()
					.filter(|(other_index, _)| *other_index != index)
					.map(|(_, (_, other_atoms))| other_atoms),
			);
			subtract_bag(atoms, &others).0
		})
		.collect();
	let relationship = if unique_bags.iter().any(BTreeMap::is_empty) {
		Relationship::Subset
	} else if jaccard >= 0.5 {
		Relationship::Redundant
	} else {
		Relationship::Disjoint
	};

	const T: f64 = 0.5;
	let contributors: Vec<ContributorRetention> = source_bags
		.iter()
		.zip(&unique_bags)
		.map(|((source, _), unique)| {
			let unique_atoms = bag_size(unique);
			let retained_unique_atoms = intersection_bag_size(unique, &human);
			let fraction_kept =
				(unique_atoms > 0).then_some(retained_unique_atoms as f64 / unique_atoms as f64);
			ContributorRetention {
				source_id: source.id.to_string(),
				unique_atoms,
				retained_unique_atoms,
				fraction_kept,
			}
		})
		.collect();
	let kept: Vec<bool> = contributors
		.iter()
		.map(|contributor| {
			contributor
				.fraction_kept
				.is_none_or(|fraction| fraction >= T)
		})
		.collect();
	let active: Vec<usize> = contributors
		.iter()
		.enumerate()
		.filter(|(_, contributor)| contributor.unique_atoms > 0)
		.map(|(index, _)| index)
		.collect();
	let active_kept: Vec<bool> = active.iter().map(|index| kept[*index]).collect();

	let verdict = if active.is_empty() {
		ResVerdict::Identical
	} else if active_kept.iter().all(|kept| *kept) {
		match active.as_slice() {
			[0] if contributors.len() == 2 => ResVerdict::TookBase,
			[1] if contributors.len() == 2 => ResVerdict::TookOverlay,
			_ => ResVerdict::Union,
		}
	} else if active_kept.iter().all(|kept| !*kept) {
		ResVerdict::HandEdit
	} else if contributors.len() == 2 {
		match (kept[0], kept[1]) {
			(true, false) => ResVerdict::TookBase,
			(false, true) => ResVerdict::TookOverlay,
			_ => unreachable!("all and none cases handled above"),
		}
	} else {
		ResVerdict::PartialUnion
	};

	let round2 = |v: f64| (v * 100.0).round() / 100.0;
	let contributors = contributors
		.into_iter()
		.map(|mut contributor| {
			contributor.fraction_kept = contributor.fraction_kept.map(round2);
			contributor
		})
		.collect();
	let human_only_atoms = bag_size(&subtract_bag(&human, &source_union).0);
	Some(Resolution {
		contributors,
		source_jaccard: round2(jaccard),
		human_only_atoms,
		basegame_atoms_subtracted,
		relationship,
		verdict,
	})
}

fn semantic_atoms_for_path(rel: &str, path: &Path) -> Option<AtomBag> {
	semantic_atoms_for_path_with_ordering(rel, path, None)
}

fn semantic_atoms_for_path_with_ordering(
	rel: &str,
	path: &Path,
	ordering: Option<AstOrderingPolicy>,
) -> Option<AtomBag> {
	if !path.is_file() {
		return None;
	}
	let extension = path
		.extension()
		.and_then(|extension| extension.to_str())
		.map(str::to_ascii_lowercase)
		.unwrap_or_default();
	match extension.as_str() {
		"yml" | "yaml" => return localisation_atoms(path),
		"csv" => return csv_atoms(path),
		"json" => return json_atoms(path),
		_ => {}
	}
	if is_clausewitz_like_path(rel) {
		let parsed = parse_clausewitz_file(path);
		if parsed.diagnostics.is_empty() {
			let ordering = ordering.unwrap_or_else(|| {
				if is_gui_like_path(rel) {
					AstOrderingPolicy::OrderSensitive
				} else {
					AstOrderingPolicy::OrderInsensitive
				}
			});
			let mut atoms = AtomBag::new();
			flatten_semantic_statements(&parsed.ast.statements, ordering, &[], &mut atoms);
			return Some(atoms);
		}
	}
	let text = read(path)?;
	let mut atoms = AtomBag::new();
	for record in normalise(&text) {
		*atoms.entry(format!("record:{record}")).or_default() += 1;
	}
	Some(atoms)
}

fn localisation_atoms(path: &Path) -> Option<AtomBag> {
	let raw = fs::read(path).ok()?;
	let text = foch_core::decode_paradox_bytes(&raw);
	let mut atoms = AtomBag::new();
	for line in text.lines() {
		let line = strip_comment_outside_quotes(line).trim();
		let Some((key, raw_value)) = line.split_once(':') else {
			continue;
		};
		let key = key.trim().trim_start_matches('\u{feff}');
		if key.is_empty() {
			continue;
		}
		let mut value = raw_value.trim();
		let version_len = value.bytes().take_while(u8::is_ascii_digit).count();
		if version_len > 0 {
			value = value[version_len..].trim_start();
		}
		if value.is_empty() && key.starts_with("l_") {
			continue;
		}
		let value = serde_json::from_str::<String>(value)
			.unwrap_or_else(|_| value.trim_matches('"').to_string());
		*atoms
			.entry(format!("localisation:{key}={value}"))
			.or_default() += 1;
	}
	Some(atoms)
}

fn csv_atoms(path: &Path) -> Option<AtomBag> {
	let raw = fs::read(path).ok()?;
	let text = foch_core::decode_paradox_bytes(&raw);
	let delimiter = text
		.lines()
		.find(|line| !line.trim().is_empty())
		.map(|line| {
			if delimiter_count(line, ';') > delimiter_count(line, ',') {
				';'
			} else {
				','
			}
		})
		.unwrap_or(',');
	let mut atoms = AtomBag::new();
	for line in text.lines().filter(|line| !line.trim().is_empty()) {
		let fields = parse_delimited_record(line.trim_start_matches('\u{feff}'), delimiter);
		let record = serde_json::to_string(&fields).expect("CSV fields serialize");
		*atoms.entry(format!("csv:{record}")).or_default() += 1;
	}
	Some(atoms)
}

fn json_atoms(path: &Path) -> Option<AtomBag> {
	let value = serde_json::from_slice::<serde_json::Value>(&fs::read(path).ok()?).ok()?;
	let mut atoms = AtomBag::new();
	flatten_json_value(&value, "$", &mut atoms);
	Some(atoms)
}

fn flatten_json_value(value: &serde_json::Value, path: &str, atoms: &mut AtomBag) {
	match value {
		serde_json::Value::Object(object) if object.is_empty() => {
			*atoms.entry(format!("json:{path}={{}}")).or_default() += 1;
		}
		serde_json::Value::Object(object) => {
			let mut keys: Vec<&String> = object.keys().collect();
			keys.sort();
			for key in keys {
				flatten_json_value(&object[key], &format!("{path}.{key}"), atoms);
			}
		}
		serde_json::Value::Array(array) if array.is_empty() => {
			*atoms.entry(format!("json:{path}=[]")).or_default() += 1;
		}
		serde_json::Value::Array(array) => {
			for (index, item) in array.iter().enumerate() {
				flatten_json_value(item, &format!("{path}[{index}]"), atoms);
			}
		}
		_ => {
			*atoms.entry(format!("json:{path}={value}")).or_default() += 1;
		}
	}
}

fn strip_comment_outside_quotes(line: &str) -> &str {
	let mut quoted = false;
	let mut escaped = false;
	for (index, character) in line.char_indices() {
		if escaped {
			escaped = false;
			continue;
		}
		match character {
			'\\' if quoted => escaped = true,
			'"' => quoted = !quoted,
			'#' if !quoted => return &line[..index],
			_ => {}
		}
	}
	line
}

fn delimiter_count(line: &str, delimiter: char) -> usize {
	let mut count = 0_usize;
	let mut quoted = false;
	for character in line.chars() {
		match character {
			'"' => quoted = !quoted,
			character if character == delimiter && !quoted => count += 1,
			_ => {}
		}
	}
	count
}

fn parse_delimited_record(line: &str, delimiter: char) -> Vec<String> {
	let mut fields = Vec::new();
	let mut field = String::new();
	let mut characters = line.chars().peekable();
	let mut quoted = false;
	while let Some(character) = characters.next() {
		match character {
			'"' if quoted && characters.peek() == Some(&'"') => {
				field.push('"');
				characters.next();
			}
			'"' => quoted = !quoted,
			character if character == delimiter && !quoted => {
				fields.push(field.trim().to_string());
				field.clear();
			}
			_ => field.push(character),
		}
	}
	fields.push(field.trim().to_string());
	fields
}

fn basegame_atoms_for_path(rel: &str, root: &Path) -> AtomBag {
	semantic_atoms_for_path(rel, &root.join(rel)).unwrap_or_default()
}

fn flatten_semantic_statements(
	statements: &[AstStatement],
	ordering: AstOrderingPolicy,
	prefix: &[String],
	atoms: &mut AtomBag,
) {
	for (index, statement) in statements.iter().enumerate() {
		let position = (ordering == AstOrderingPolicy::OrderSensitive).then_some(index);
		match statement {
			AstStatement::Assignment { key, value, .. } => {
				let mut path = prefix.to_vec();
				path.push(match position {
					Some(index) => format!("assignment:{key}@{index}"),
					None => format!("assignment:{key}"),
				});
				flatten_semantic_value(value, ordering, &path, atoms);
			}
			AstStatement::Item { value, .. } => {
				let mut path = prefix.to_vec();
				path.push(match position {
					Some(index) => format!("item@{index}"),
					None => "item".to_string(),
				});
				flatten_semantic_value(value, ordering, &path, atoms);
			}
			AstStatement::Comment { .. } => {}
		}
	}
}

fn flatten_semantic_value(
	value: &AstValue,
	ordering: AstOrderingPolicy,
	path: &[String],
	atoms: &mut AtomBag,
) {
	match value {
		AstValue::Scalar { value, .. } => {
			*atoms
				.entry(format!("{}={}", path.join("/"), canonical_scalar(value)))
				.or_default() += 1;
		}
		AstValue::Block { items, .. } if items.is_empty() => {
			*atoms.entry(format!("{}={{}}", path.join("/"))).or_default() += 1;
		}
		AstValue::Block { items, .. } => {
			flatten_semantic_statements(items, ordering, path, atoms);
		}
	}
}

fn subtract_bag(left: &AtomBag, right: &AtomBag) -> (AtomBag, usize) {
	let mut result = AtomBag::new();
	let mut removed = 0_usize;
	for (atom, left_count) in left {
		let right_count = right.get(atom).copied().unwrap_or(0);
		let kept = left_count.saturating_sub(right_count);
		removed += left_count - kept;
		if kept > 0 {
			result.insert(atom.clone(), kept);
		}
	}
	(result, removed)
}

fn union_bags<'a>(bags: impl Iterator<Item = &'a AtomBag>) -> AtomBag {
	let mut union = AtomBag::new();
	for bag in bags {
		for (atom, count) in bag {
			let slot = union.entry(atom.clone()).or_default();
			*slot = (*slot).max(*count);
		}
	}
	union
}

fn intersection_size<'a>(mut bags: impl Iterator<Item = &'a AtomBag>) -> usize {
	let Some(first) = bags.next() else {
		return 0;
	};
	let mut intersection = first.clone();
	for bag in bags {
		intersection.retain(|atom, count| {
			*count = (*count).min(bag.get(atom).copied().unwrap_or(0));
			*count > 0
		});
	}
	bag_size(&intersection)
}

fn intersection_bag_size(left: &AtomBag, right: &AtomBag) -> usize {
	left.iter()
		.map(|(atom, count)| (*count).min(right.get(atom).copied().unwrap_or(0)))
		.sum()
}

fn bag_size(bag: &AtomBag) -> usize {
	bag.values().sum()
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod classify_tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	const GOVERNMENTS_OUTPUT: &str = "common/governments/zzz_foch_governments.txt";

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

	fn classify_two(rel: &str, base: &Path, overlay: &Path, compatch: &Path) -> Option<Resolution> {
		let sources = two_sources(base, overlay);
		classify_resolution(rel, &sources, compatch, None)
	}

	fn two_sources<'a>(base: &'a Path, overlay: &'a Path) -> [SourceMod<'a>; 2] {
		[
			SourceMod {
				id: "base",
				root: base,
			},
			SourceMod {
				id: "overlay",
				root: overlay,
			},
		]
	}

	fn write_governments_replace_descriptor(root: &Path) {
		write_file(
			root,
			"descriptor.mod",
			"name=\"fixture\"\nreplace_path=\"common/governments\"\n",
		);
	}

	#[test]
	fn review_evidence_binds_all_runtime_layers() {
		let basegame = tempfile::tempdir().unwrap();
		let left = tempfile::tempdir().unwrap();
		let right = tempfile::tempdir().unwrap();
		let compatch = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let rel = "decisions/example.txt";
		write_file(basegame.path(), rel, "decision = { value = 1 }\n");
		write_file(left.path(), rel, "decision = { value = 2 }\n");
		write_file(right.path(), rel, "decision = { value = 1 flag = yes }\n");
		let merged = "decision = { value = 2 flag = yes }\n";
		write_file(compatch.path(), rel, merged);
		write_file(output.path(), rel, merged);
		let sources = two_sources(left.path(), right.path());
		let conflicts = HashSet::new();

		let evidence = review_semantic_evidence(
			&ScoreFileRequest {
				rel,
				source_mods: &sources,
				compatch: compatch.path(),
				out_dir: output.path(),
				conflict_paths: &conflicts,
			},
			Some(basegame.path()),
		)
		.expect("review evidence");

		assert_eq!(evidence.sources.len(), 2);
		assert_eq!(
			evidence.candidate.semantic_content_id,
			evidence.human.semantic_content_id
		);
		assert!(evidence.candidate_vs_human.left_only.is_empty());
		assert!(evidence.candidate_vs_human.right_only.is_empty());
		assert!(
			evidence
				.sources
				.iter()
				.all(|source| !source.vs_base.left_only.is_empty())
		);
	}

	#[test]
	fn review_evidence_uses_complete_definition_module_views() {
		let basegame = tempfile::tempdir().unwrap();
		let left = tempfile::tempdir().unwrap();
		let right = tempfile::tempdir().unwrap();
		let compatch = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		write_file(
			basegame.path(),
			"common/governments/00_base.txt",
			"shared = { rank = 1 }\n",
		);
		write_file(
			left.path(),
			"common/governments/zzz_left.txt",
			"shared = { rank = 2 }\n",
		);
		write_file(
			right.path(),
			"common/governments/10_right.txt",
			"right_only = { rank = 1 }\n",
		);
		let merged = "right_only = { rank = 1 }\nshared = { rank = 2 }\n";
		write_file(compatch.path(), "common/governments/00_human.txt", merged);
		write_file(output.path(), GOVERNMENTS_OUTPUT, merged);
		let sources = two_sources(left.path(), right.path());
		let conflicts = HashSet::new();

		let evidence = review_semantic_evidence(
			&ScoreFileRequest {
				rel: GOVERNMENTS_OUTPUT,
				source_mods: &sources,
				compatch: compatch.path(),
				out_dir: output.path(),
				conflict_paths: &conflicts,
			},
			Some(basegame.path()),
		)
		.expect("module review evidence");

		assert_eq!(
			evidence.candidate.semantic_content_id,
			evidence.human.semantic_content_id
		);
		assert_eq!(evidence.candidate.atoms, evidence.human.atoms);
		assert!(evidence.candidate_vs_human.left_only.is_empty());
		assert!(evidence.candidate_vs_human.right_only.is_empty());
	}

	#[test]
	fn cr_identical() {
		let (b, o, c) = make_dirs();
		let content = "a = 1\nb = 2\n";
		write_file(b.path(), "f.txt", content);
		write_file(o.path(), "f.txt", content);
		write_file(c.path(), "f.txt", content);
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::Identical, "verdict");
		assert_eq!(res.relationship, Relationship::Subset, "relationship");
		assert_eq!(res.source_jaccard, 1.0, "jaccard");
		assert_eq!(res.contributors[0].fraction_kept, None, "fa");
		assert_eq!(res.contributors[1].fraction_kept, None, "fb");
	}

	#[test]
	fn cr_union() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps both unique lines
		write_file(c.path(), "f.txt", "common = 1\nx = 1\ny = 2\n");
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::Union, "verdict");
		assert_eq!(res.relationship, Relationship::Disjoint, "relationship");
		assert_eq!(res.contributors[0].fraction_kept, Some(1.0), "fa");
		assert_eq!(res.contributors[1].fraction_kept, Some(1.0), "fb");
	}

	#[test]
	fn cr_took_base() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps only base's unique line
		write_file(c.path(), "f.txt", "common = 1\nx = 1\n");
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::TookBase, "verdict");
		assert_eq!(res.contributors[0].fraction_kept, Some(1.0), "fa");
		assert_eq!(res.contributors[1].fraction_kept, Some(0.0), "fb");
	}

	#[test]
	fn cr_took_overlay() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps only overlay's unique line
		write_file(c.path(), "f.txt", "common = 1\ny = 2\n");
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::TookOverlay, "verdict");
		assert_eq!(res.contributors[0].fraction_kept, Some(0.0), "fa");
		assert_eq!(res.contributors[1].fraction_kept, Some(1.0), "fb");
	}

	#[test]
	fn cr_hand_edit() {
		let (b, o, c) = make_dirs();
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\ny = 2\n");
		// compatch keeps neither side's unique line
		write_file(c.path(), "f.txt", "common = 1\nz = 3\n");
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.verdict, ResVerdict::HandEdit, "verdict");
		assert_eq!(res.contributors[0].fraction_kept, Some(0.0), "fa");
		assert_eq!(res.contributors[1].fraction_kept, Some(0.0), "fb");
	}

	#[test]
	fn cr_missing_file_returns_none() {
		let (b, _o, c) = make_dirs();
		write_file(b.path(), "f.txt", "a = 1\n");
		// overlay file is absent → None
		write_file(c.path(), "f.txt", "a = 1\n");
		let res = classify_two("f.txt", b.path(), _o.path(), c.path());
		assert!(res.is_none(), "expect None when a file is missing");
	}

	#[test]
	fn cr_subset_relationship() {
		let (b, o, c) = make_dirs();
		// overlay is a subset of base (b_only is empty)
		write_file(b.path(), "f.txt", "common = 1\nx = 1\n");
		write_file(o.path(), "f.txt", "common = 1\n");
		write_file(c.path(), "f.txt", "common = 1\nx = 1\n");
		let res = classify_two("f.txt", b.path(), o.path(), c.path()).unwrap();
		assert_eq!(res.relationship, Relationship::Subset, "relationship");
		assert_eq!(
			res.verdict,
			ResVerdict::TookBase,
			"verdict (subset, kept base unique)"
		);
		assert_eq!(res.contributors[0].fraction_kept, Some(1.0), "fa");
		assert_eq!(
			res.contributors[1].fraction_kept, None,
			"fb (no overlay unique atoms)"
		);
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
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
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
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::DivergesAst);
		assert_eq!(score.acceptance_reason, None);
	}

	#[test]
	fn score_file_uses_highest_precedence_source_when_output_is_omitted() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/frontend.gui";
		write_file(mod_a.path(), rel, "guiTypes = { active = no }\n");
		write_file(mod_b.path(), rel, "guiTypes = { active = yes }\n");
		write_file(compatch.path(), rel, "guiTypes = { active = yes }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert!(!score.foch_emitted);
		assert_eq!(score.ast_match, Some(true));
		assert_eq!(score.verdict, Verdict::MatchesHuman);
	}

	#[test]
	fn score_file_falls_back_to_basegame_when_output_and_sources_are_omitted() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let basegame = tempfile::tempdir().unwrap();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/frontend.gui";
		write_file(basegame.path(), rel, "guiTypes = { active = yes }\n");
		write_file(compatch.path(), rel, "guiTypes = { active = yes }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());
		let request = ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		};

		let score = score_file_with_basegame(&request, Some(basegame.path()));

		assert!(!score.foch_emitted);
		assert_eq!(score.source_count, 0);
		assert_eq!(score.ast_match, Some(true));
		assert_eq!(score.verdict, Verdict::MatchesHuman);
	}

	#[test]
	fn score_file_respects_replace_path_when_output_is_omitted() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		let rel = "interface/frontend.gui";
		write_file(mod_a.path(), rel, "guiTypes = { active = yes }\n");
		write_file(
			mod_b.path(),
			"descriptor.mod",
			"name=\"replacement\"\nreplace_path=\"interface\"\n",
		);
		write_file(compatch.path(), rel, "guiTypes = { active = yes }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert!(!score.foch_emitted);
		assert_eq!(score.ast_match, None);
		assert_eq!(score.verdict, Verdict::NotEmitted);
	}

	#[test]
	fn score_file_accepts_exact_definition_module_split_across_filenames() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(out.path());
		write_file(
			mod_a.path(),
			"common/governments/a_governments.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			mod_b.path(),
			"common/governments/b_governments.txt",
			"republic = { rank = 2 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/00_human.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/10_human.txt",
			"republic = { rank = 2 }\n",
		);
		write_file(
			out.path(),
			GOVERNMENTS_OUTPUT,
			"republic = { rank = 2 }\nmonarchy = { rank = 1 }\n",
		);
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel: GOVERNMENTS_OUTPUT,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert_eq!(score.source_mod_ids, ["base", "overlay"]);
		assert!(score.foch_emitted);
		assert_eq!(score.keys_match, Some(true));
		assert_eq!(score.ast_match, Some(true));
		assert_eq!(score.verdict, Verdict::AcceptedEquivalent);
		assert_eq!(
			score.acceptance_reason.as_deref(),
			Some("same_family_module_equivalent")
		);
	}

	#[test]
	fn score_file_compares_layered_runtime_module_views() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let basegame = tempfile::tempdir().unwrap();
		let out = tempfile::tempdir().unwrap();
		write_file(
			basegame.path(),
			"common/governments/00_vanilla.txt",
			"monarchy = { rank = 0 }\nvanilla_only = { rank = 0 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/99_human_override.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_governments_replace_descriptor(out.path());
		write_file(
			out.path(),
			GOVERNMENTS_OUTPUT,
			"vanilla_only = { rank = 0 }\nmonarchy = { rank = 1 }\n",
		);
		let sources = two_sources(mod_a.path(), mod_b.path());
		let request = ScoreFileRequest {
			rel: GOVERNMENTS_OUTPUT,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		};

		let score = score_file_with_basegame(&request, Some(basegame.path()));

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert_eq!(score.keys_match, Some(true));
		assert_eq!(score.ast_match, Some(true));
		assert_eq!(score.verdict, Verdict::AcceptedEquivalent);
	}

	#[test]
	fn score_file_prefers_later_layer_over_earlier_lexically_later_definition() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_file(
			mod_a.path(),
			"common/governments/zzz_source.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/00_compatch.txt",
			"monarchy = { rank = 2 }\n",
		);
		write_governments_replace_descriptor(out.path());
		write_file(out.path(), GOVERNMENTS_OUTPUT, "monarchy = { rank = 2 }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel: GOVERNMENTS_OUTPUT,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.keys_match, Some(true));
		assert_eq!(score.ast_match, Some(true));
		assert_eq!(score.verdict, Verdict::AcceptedEquivalent);
	}

	#[test]
	fn score_file_rejects_extra_definition_in_complete_module() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(out.path());
		let rel = "common/governments/00_governments.txt";
		let monarchy = "monarchy = { rank = 1 }\n";
		write_file(mod_a.path(), rel, monarchy);
		write_file(mod_b.path(), rel, monarchy);
		write_file(compatch.path(), rel, monarchy);
		write_file(out.path(), GOVERNMENTS_OUTPUT, monarchy);
		write_file(
			out.path(),
			"common/governments/stale_governments.txt",
			"stale_only = { rank = 0 }\n",
		);
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert_eq!(score.keys_match, Some(false));
		assert_eq!(score.ast_match, Some(false));
		assert!(score.dropped_keys.is_empty());
		assert_eq!(score.verdict, Verdict::DivergesStructure);
		assert_eq!(score.acceptance_reason, None);
	}

	#[test]
	fn score_file_reports_all_missing_human_module_keys() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(out.path());
		write_file(
			mod_a.path(),
			"common/governments/a_governments.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			mod_b.path(),
			"common/governments/b_governments.txt",
			"republic = { rank = 2 }\ntribal = { rank = 0 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/00_human.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			compatch.path(),
			"common/governments/10_human.txt",
			"republic = { rank = 2 }\ntribal = { rank = 0 }\n",
		);
		write_file(out.path(), GOVERNMENTS_OUTPUT, "monarchy = { rank = 1 }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel: GOVERNMENTS_OUTPUT,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert_eq!(score.source_mod_ids, ["base", "overlay"]);
		assert_eq!(score.keys_match, Some(false));
		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.dropped_keys, ["republic", "tribal"]);
		assert_eq!(score.verdict, Verdict::DropsContent);
	}

	#[test]
	fn score_file_reports_changed_module_value_as_ast_divergence() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(out.path());
		let rel = "common/governments/human.txt";
		write_file(mod_a.path(), rel, "monarchy = { rank = 1 }\n");
		write_file(mod_b.path(), rel, "monarchy = { rank = 2 }\n");
		write_file(compatch.path(), rel, "monarchy = { rank = 1 }\n");
		write_file(out.path(), GOVERNMENTS_OUTPUT, "monarchy = { rank = 2 }\n");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert_eq!(score.keys_match, Some(true));
		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::DivergesAst);
	}

	#[test]
	fn score_file_rejects_definition_module_loader_failure_on_either_side() {
		let (mod_a, mod_b, human_invalid) = make_dirs();
		let human_valid = tempfile::tempdir().unwrap();
		let foch_valid = tempfile::tempdir().unwrap();
		let foch_invalid = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(foch_valid.path());
		write_governments_replace_descriptor(foch_invalid.path());
		let rel = "common/governments/human.txt";
		let monarchy = "monarchy = { rank = 1 }\n";
		write_file(mod_a.path(), rel, monarchy);
		write_file(mod_b.path(), rel, monarchy);
		write_file(human_invalid.path(), rel, monarchy);
		write_file(
			human_invalid.path(),
			"common/governments/invalid_sibling.txt",
			"unexpected_item\n",
		);
		write_file(human_valid.path(), rel, monarchy);
		write_file(foch_valid.path(), GOVERNMENTS_OUTPUT, monarchy);
		write_file(foch_invalid.path(), GOVERNMENTS_OUTPUT, monarchy);
		write_file(
			foch_invalid.path(),
			"common/governments/invalid_sibling.txt",
			"unexpected_item\n",
		);
		let sources = two_sources(mod_a.path(), mod_b.path());

		let human_loader_failure = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: human_invalid.path(),
			out_dir: foch_valid.path(),
			conflict_paths: &HashSet::new(),
		});
		let foch_loader_failure = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: human_valid.path(),
			out_dir: foch_invalid.path(),
			conflict_paths: &HashSet::new(),
		});

		for score in [human_loader_failure, foch_loader_failure] {
			assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
			assert_eq!(score.keys_match, None);
			assert_eq!(score.ast_match, None);
			assert_eq!(score.verdict, Verdict::DivergesStructure);
			assert!(!score.verdict.accepted_ok());
		}
	}

	#[test]
	fn conflict_paths_exclude_typed_unsupported_and_engine_failures() {
		let mut report = MergeReport::default();
		for (path, deferred_reason) in [
			(
				"common/actual_conflict.txt",
				DeferredUnitReason::NeedsUserChoice,
			),
			(
				"common/unsupported.txt",
				DeferredUnitReason::UnsupportedInput,
			),
			(
				"common/engine_failure.txt",
				DeferredUnitReason::EngineFailure,
			),
		] {
			report
				.conflict_resolutions
				.push(foch_core::model::MergeReportConflictResolution {
					path: path.to_string(),
					reason: deferred_reason.as_str().to_string(),
					deferred_reason,
					kind: None,
					leaf_conflicts: Vec::new(),
				});
		}
		report.warnings.push(
			"engine failure for common/engine_failure.txt; deferred, skipping output".to_string(),
		);

		assert_eq!(
			conflict_rel_paths(&report),
			HashSet::from(["common/actual_conflict.txt".to_string()])
		);
	}

	#[test]
	fn score_file_uses_fixed_module_output_path_for_conflicts() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_governments_replace_descriptor(out.path());
		let rel = "common/governments/human.txt";
		let monarchy = "monarchy = { rank = 1 }\n";
		write_file(mod_a.path(), rel, monarchy);
		write_file(mod_b.path(), rel, monarchy);
		write_file(compatch.path(), rel, monarchy);
		write_file(out.path(), GOVERNMENTS_OUTPUT, monarchy);
		let sources = two_sources(mod_a.path(), mod_b.path());
		let conflicts = HashSet::from([GOVERNMENTS_OUTPUT.to_string()]);

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &conflicts,
		});

		assert_eq!(score.rel, GOVERNMENTS_OUTPUT);
		assert!(score.foch_conflict);
		assert_eq!(score.verdict, Verdict::ConflictWithheld);
	}

	#[test]
	fn retained_paths_leave_module_expansion_to_the_engine() {
		let source = tempfile::tempdir().unwrap();
		write_file(
			source.path(),
			"common/governments/zzz_governments.txt",
			"monarchy = {}\n",
		);
		write_file(source.path(), "interface/unrelated.gui", "guiTypes = {}\n");
		let ground_truth = vec![
			"common/governments/00_governments.txt".to_string(),
			"interface/target.gui".to_string(),
		];

		let retained = scoring_requested_paths(&ground_truth);

		assert_eq!(retained, ground_truth.into_iter().collect());
		assert!(!retained.contains("interface/unrelated.gui"));
	}

	#[test]
	fn scoring_reference_units_collapse_definition_module_paths_deterministically() {
		let forward = vec![
			"common/governments/10_human.txt".to_string(),
			"interface/target.gui".to_string(),
			"common/governments/00_human.txt".to_string(),
			"music/not-a-unit.ogg".to_string(),
		];
		let reverse = forward.iter().rev().cloned().collect::<Vec<_>>();
		let expected = vec![
			GOVERNMENTS_OUTPUT.to_string(),
			"interface/target.gui".to_string(),
		];

		assert_eq!(scoring_reference_units(&forward), expected);
		assert_eq!(scoring_reference_units(&reverse), expected);
	}

	#[test]
	fn scoring_evidence_expands_modules_but_keeps_path_units_exact() {
		let root = tempfile::tempdir().unwrap();
		write_file(
			root.path(),
			"descriptor.mod",
			"name=\"fixture\"\nreplace_path=\"common/governments\"\n",
		);
		write_file(root.path(), "common/governments/a.txt", "monarchy = {}\n");
		write_file(
			root.path(),
			"common/governments/nested/b.txt",
			"republic = {}\n",
		);
		write_file(
			root.path(),
			"common/governments/nested/sound.ogg",
			"\0binary",
		);
		write_file(root.path(), "common/governments/unknown.bin", "\0binary");
		write_file(root.path(), "interface/target.gui", "guiTypes = {}\n");
		write_file(root.path(), "interface/unrelated.gui", "guiTypes = {}\n");

		assert_eq!(
			scoring_evidence_files(root.path(), GOVERNMENTS_OUTPUT).unwrap(),
			vec![
				"common/governments/a.txt".to_string(),
				"common/governments/nested/b.txt".to_string(),
				"descriptor.mod".to_string(),
			]
		);
		assert_eq!(
			scoring_evidence_files(root.path(), "interface/target.gui").unwrap(),
			vec![
				"descriptor.mod".to_string(),
				"interface/target.gui".to_string(),
			]
		);
	}

	#[test]
	fn unsupported_binary_paths_cannot_enter_scoring_or_evidence_units() {
		let root = tempfile::tempdir().unwrap();
		for relative in [
			"music/reference.ogg",
			"sound/reference.wav",
			"gfx/models/reference.mesh",
			"gfx/models/reference.anim",
			"common/reference.unknown",
			"not_loaded/reference.txt",
		] {
			assert!(scoring_reference_units(&[relative.to_string()]).is_empty());
			assert!(scoring_evidence_files(root.path(), relative).is_err());
			assert!(!scoring_evidence_path_belongs_to_unit(relative, relative));
		}
		assert!(!scoring_evidence_path_belongs_to_unit(
			GOVERNMENTS_OUTPUT,
			"common/governments/reference.mesh"
		));
	}

	#[test]
	fn canonical_module_view_rejects_unsupported_top_level_items() {
		let root = tempfile::tempdir().unwrap();
		write_file(
			root.path(),
			"common/governments/invalid.txt",
			"unexpected_item\nvalid = yes\n",
		);

		assert!(canonical_module_view_uncached(root.path(), "common/governments").is_none());
	}

	#[test]
	fn canonical_module_view_keys_repeated_wrappers_by_inner_field() {
		let root = tempfile::tempdir().unwrap();
		write_file(
			root.path(),
			"common/estates_preload/modifiers.txt",
			"modifier = { key = estate_balance add_loyalty = 5 }\n\
			 modifier = { key = estate_support add_influence = 2 }\n",
		);

		let view = canonical_module_view_uncached(root.path(), "common/estates_preload")
			.expect("estates preload module view");
		assert_eq!(view.len(), 2);
		assert!(view.contains_key("modifier:estate_balance"));
		assert!(view.contains_key("modifier:estate_support"));
	}

	#[test]
	fn module_file_collection_propagates_walk_errors() {
		let root = tempfile::tempdir().unwrap();
		let missing = root.path().join("missing-module-directory");

		assert!(
			collect_module_files(root.path(), walkdir::WalkDir::new(missing)).is_none(),
			"a failed walk must not become an incomplete successful module view"
		);
	}

	#[test]
	fn score_file_includes_uppercase_extension_in_definition_module() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let out = tempfile::tempdir().unwrap();
		write_file(
			compatch.path(),
			"common/governments/HUMAN.TXT",
			"monarchy = { rank = 1 }\n",
		);
		write_governments_replace_descriptor(out.path());
		write_file(out.path(), GOVERNMENTS_OUTPUT, "");
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel: GOVERNMENTS_OUTPUT,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.dropped_keys, ["monarchy"]);
		assert_eq!(score.verdict, Verdict::DropsContent);
	}

	#[test]
	fn layered_module_cache_preserves_root_order() {
		let earlier = tempfile::tempdir().unwrap();
		let later = tempfile::tempdir().unwrap();
		let rel = "common/governments/shared.txt";
		write_file(earlier.path(), rel, "monarchy = { rank = 1 }\n");
		write_file(later.path(), rel, "monarchy = { rank = 2 }\n");
		let mut cache = ScoreCache::new();

		let forward = cache
			.module_view(&[earlier.path(), later.path()], "common/governments")
			.unwrap();
		let reverse = cache
			.module_view(&[later.path(), earlier.path()], "common/governments")
			.unwrap();

		assert_ne!(forward, reverse);
	}

	#[test]
	fn layered_module_cache_tracks_file_and_descriptor_content() {
		let earlier = tempfile::tempdir().unwrap();
		let later = tempfile::tempdir().unwrap();
		write_file(
			earlier.path(),
			"common/governments/earlier.txt",
			"monarchy = { rank = 1 }\n",
		);
		write_file(
			later.path(),
			"common/governments/later.txt",
			"republic = { rank = 1 }\n",
		);
		let mut cache = ScoreCache::new();
		let roots = [earlier.path(), later.path()];

		let initial = cache.module_view(&roots, "common/governments").unwrap();
		write_file(
			later.path(),
			"common/governments/later.txt",
			"republic = { rank = 2 }\n",
		);
		let changed_file = cache.module_view(&roots, "common/governments").unwrap();
		write_governments_replace_descriptor(later.path());
		let changed_descriptor = cache.module_view(&roots, "common/governments").unwrap();

		assert_ne!(initial, changed_file);
		assert!(changed_file.contains_key("monarchy"));
		assert!(!changed_descriptor.contains_key("monarchy"));
		assert!(changed_descriptor.contains_key("republic"));
	}

	#[test]
	fn layered_module_cache_rejects_a_change_during_view_construction() {
		let root = tempfile::tempdir().unwrap();
		let rel = "common/governments/governments.txt";
		write_file(root.path(), rel, "monarchy = { rank = 1 }\n");
		let mut cache = ScoreCache::new();

		let view = cache.module_view_with_post_build(&[root.path()], "common/governments", || {
			write_file(root.path(), rel, "monarchy = { rank = 2 }\n")
		});

		assert!(view.is_none());
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
		let sources = two_sources(mod_a.path(), mod_b.path());

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.ast_match, Some(false));
		assert_eq!(score.verdict, Verdict::DivergesAst);
		assert_eq!(score.acceptance_reason, None);
	}

	#[test]
	fn module_family_eligibility_keeps_path_sensitive_roots_out() {
		assert!(eligible_module_family("common/governments/00_governments.txt").is_some());
		assert!(eligible_module_family("common/estates_preload/modifiers.txt").is_some());
		assert!(eligible_module_family("common/defines/example.lua").is_none());
		assert!(eligible_module_family("common/interface/example.gui").is_none());
		assert!(eligible_module_family("common/countries/FRA.txt").is_none());
		assert!(eligible_module_family("common/units/infantry.txt").is_none());
		assert!(eligible_module_family("interface/example.gui").is_none());
		assert!(eligible_module_family("history/countries/FRA - France.txt").is_none());
		assert!(eligible_module_family("common/technology.txt").is_none());
	}

	#[test]
	fn accepted_better_requires_exact_snapshot_unit_and_semantic_evidence() {
		let temp = tempfile::tempdir().unwrap();
		let rel = "common/scripted_triggers/example.txt";
		let candidate_path = temp.path().join("candidate.txt");
		let human_path = temp.path().join("human.txt");
		write_file(temp.path(), "candidate.txt", "trigger = { tag = ENG }\n");
		write_file(temp.path(), "human.txt", "trigger = { tag = FRA }\n");
		let candidate = parse_clausewitz_file(&candidate_path).ast;
		let human = parse_clausewitz_file(&human_path).ast;
		let candidate_atoms = semantic_atom_bag_statements(&candidate.statements);
		let (required_atom, required_count) = candidate_atoms
			.iter()
			.next()
			.expect("candidate has an atom");
		let human_only = semantic_atom_bag_diff(
			&semantic_atom_bag_statements(&human.statements),
			&candidate_atoms,
		)
		.left_only
		.into_keys()
		.next()
		.expect("human has a distinct atom");
		let fixture = serde_json::json!([{
			"compatch_id": "case",
			"rel": rel,
			"snapshot_id": "snapshot",
			"scoring_unit_id": "unit",
			"verdict": "accepted_better",
			"reason": "candidate preserves the intended corrected country tag",
			"candidate_semantic_content_id": semantic_ast_content_id(&candidate),
			"human_semantic_content_id": semantic_ast_content_id(&human),
			"required_atoms": { (required_atom): required_count },
			"forbidden_atoms": [human_only],
		}]);
		let adjudications = Adjudications::from_json(&fixture.to_string()).unwrap();
		let binding = AdjudicationBinding {
			compatch_id: "case",
			relative_path: rel,
			snapshot_id: "snapshot",
			scoring_unit_id: "unit",
		};

		assert_eq!(
			adjudications
				.accepted_better_reason(&binding, &candidate, &human)
				.as_deref(),
			Some("candidate preserves the intended corrected country tag")
		);
		let stale_binding = AdjudicationBinding {
			snapshot_id: "new-snapshot",
			..binding
		};
		assert_eq!(
			adjudications.accepted_better_reason(&stale_binding, &candidate, &human),
			None
		);
		assert_eq!(
			adjudications.accepted_better_reason(&binding, &human, &human),
			None
		);
	}

	#[test]
	fn score_file_uses_every_source_mod_for_overlap_and_dropped_keys() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let mod_c = tempfile::tempdir().unwrap();
		let out = tempfile::tempdir().unwrap();
		let rel = "common/governments/example.txt";
		write_file(mod_a.path(), rel, "a = { rank = 1 }\n");
		write_file(mod_b.path(), rel, "b = { rank = 2 }\n");
		write_file(mod_c.path(), rel, "c = { rank = 3 }\n");
		write_file(
			compatch.path(),
			rel,
			"a = { rank = 1 }\nb = { rank = 2 }\nc = { rank = 3 }\n",
		);
		write_file(out.path(), rel, "a = { rank = 1 }\nb = { rank = 2 }\n");
		let sources = [
			SourceMod {
				id: "a",
				root: mod_a.path(),
			},
			SourceMod {
				id: "b",
				root: mod_b.path(),
			},
			SourceMod {
				id: "c",
				root: mod_c.path(),
			},
		];

		let score = score_file(&ScoreFileRequest {
			rel,
			source_mods: &sources,
			compatch: compatch.path(),
			out_dir: out.path(),
			conflict_paths: &HashSet::new(),
		});

		assert_eq!(score.source_mod_ids, vec!["a", "b", "c"]);
		assert_eq!(score.source_count, 3);
		assert!(score.multi_source);
		assert_eq!(score.dropped_keys, vec!["c"]);
		assert_eq!(score.verdict, Verdict::DropsContent);
	}

	#[test]
	fn resolution_handles_three_sources_and_subtracts_basegame_atoms() {
		let (mod_a, mod_b, compatch) = make_dirs();
		let mod_c = tempfile::tempdir().unwrap();
		let basegame = tempfile::tempdir().unwrap();
		let rel = "common/governments/example.txt";
		let vanilla = "template = { vanilla = yes }\n";
		write_file(
			basegame.path(),
			"common/governments/00_vanilla.txt",
			vanilla,
		);
		write_file(mod_a.path(), rel, &format!("{vanilla}a = 1\n"));
		write_file(mod_b.path(), rel, &format!("{vanilla}b = 2\n"));
		write_file(mod_c.path(), rel, &format!("{vanilla}c = 3\n"));
		write_file(
			compatch.path(),
			rel,
			&format!("{vanilla}a = 1\nc = 3\nhuman_fix = yes\n"),
		);
		let sources = [
			SourceMod {
				id: "a",
				root: mod_a.path(),
			},
			SourceMod {
				id: "b",
				root: mod_b.path(),
			},
			SourceMod {
				id: "c",
				root: mod_c.path(),
			},
		];

		let resolution =
			classify_resolution(rel, &sources, compatch.path(), Some(basegame.path())).unwrap();
		assert_eq!(resolution.verdict, ResVerdict::PartialUnion);
		assert_eq!(
			resolution
				.contributors
				.iter()
				.map(|contributor| contributor.fraction_kept)
				.collect::<Vec<_>>(),
			vec![Some(1.0), Some(0.0), Some(1.0)]
		);
		assert_eq!(resolution.human_only_atoms, 1);
		assert!(resolution.basegame_atoms_subtracted > 0);
	}

	#[test]
	fn resolution_uses_static_module_sources_across_filenames() {
		let mod_a = tempfile::tempdir().unwrap();
		let mod_b = tempfile::tempdir().unwrap();
		let compatch = tempfile::tempdir().unwrap();
		let basegame = tempfile::tempdir().unwrap();
		let rel = "common/governments/00_compatch.txt";
		write_file(
			basegame.path(),
			"common/governments/00_base.txt",
			"shared = { rank = 1 }\n",
		);
		write_file(
			mod_a.path(),
			"common/governments/a_governments.txt",
			"shared = { rank = 1 }\nmonarchy = { rank = 1 }\n",
		);
		write_file(
			mod_b.path(),
			"common/governments/b_governments.txt",
			"shared = { rank = 1 }\nrepublic = { rank = 2 }\n",
		);
		write_file(
			compatch.path(),
			rel,
			"shared = { rank = 1 }\nmonarchy = { rank = 1 }\nrepublic = { rank = 2 }\n",
		);
		let sources = two_sources(mod_a.path(), mod_b.path());

		let resolution = classify_resolution(rel, &sources, compatch.path(), Some(basegame.path()))
			.expect("static module definitions resolve across filenames");

		assert_eq!(resolution.basegame_atoms_subtracted, 1);
		assert_eq!(resolution.verdict, ResVerdict::Union);
		assert_eq!(resolution.contributors.len(), 2);
		assert!(
			resolution
				.contributors
				.iter()
				.all(|contributor| contributor.fraction_kept == Some(1.0))
		);
	}

	#[test]
	fn structured_non_clausewitz_atoms_ignore_format_only_differences() {
		let (left, right, _) = make_dirs();
		write_file(
			left.path(),
			"localisation/test_l_english.yml",
			"l_english:\n key:0 \"hello # world\" # note\n",
		);
		write_file(
			right.path(),
			"localisation/test_l_english.yml",
			"l_english:\nkey:1 \"hello # world\"\n",
		);
		write_file(left.path(), "map/test.csv", "\"a,b\", c\n");
		write_file(right.path(), "map/test.csv", "\"a,b\",c\n");
		write_file(left.path(), "launcher/test.json", r#"{"b":2,"a":1}"#);
		write_file(right.path(), "launcher/test.json", r#"{"a":1,"b":2}"#);

		for rel in [
			"localisation/test_l_english.yml",
			"map/test.csv",
			"launcher/test.json",
		] {
			assert_eq!(
				semantic_atoms_for_path(rel, &left.path().join(rel)),
				semantic_atoms_for_path(rel, &right.path().join(rel)),
				"structured atoms drifted for {rel}"
			);
		}
	}

	#[test]
	fn semantic_atom_diff_reports_directional_ast_leaves() {
		let (left, right, _) = make_dirs();
		let rel = "common/governments/example.txt";
		write_file(left.path(), rel, "shared = { rank = 1 }\n");
		write_file(right.path(), rel, "shared = { rank = 2 extra = yes }\n");

		let diff = semantic_atom_diff(rel, &left.path().join(rel), &right.path().join(rel), false)
			.unwrap();

		assert_eq!(diff.left_atoms, 1);
		assert_eq!(diff.right_atoms, 2);
		assert_eq!(diff.shared_atoms, 0);
		assert!(
			diff.left_only
				.keys()
				.any(|atom| atom.ends_with("rank=number:1"))
		);
		assert!(
			diff.right_only
				.keys()
				.any(|atom| atom.ends_with("rank=number:2"))
		);
		assert!(
			diff.right_only
				.keys()
				.any(|atom| atom.ends_with("extra=bool:yes"))
		);
	}

	#[test]
	fn reference_output_files_admits_only_supported_loadable_text_families() {
		let compatch = tempfile::tempdir().unwrap();
		write_file(compatch.path(), "README.md", "not a game input\n");
		write_file(compatch.path(), ".git/ignored.txt", "not a game input\n");
		write_file(
			compatch.path(),
			"not_loaded/ignored.txt",
			"not a game input\n",
		);
		write_file(
			compatch.path(),
			"common/scripted_effects/reference.txt",
			"reference = {}\n",
		);
		write_file(
			compatch.path(),
			"common/defines/00_reference.lua",
			"NGame = {}\n",
		);
		write_file(compatch.path(), "common/reference.mod", "reference = yes\n");
		write_file(compatch.path(), "events/reference.TXT", "reference = {}\n");
		write_file(
			compatch.path(),
			"interface/reference.gui",
			"guiTypes = {}\n",
		);
		write_file(
			compatch.path(),
			"interface/reference.gfx",
			"spriteTypes = {}\n",
		);
		write_file(
			compatch.path(),
			"gfx/models/reference.asset",
			"pdxmesh = {}\n",
		);
		write_file(
			compatch.path(),
			"localisation/reference_l_english.yml",
			"l_english:\n reference:0 \"Reference\"\n",
		);
		write_file(
			compatch.path(),
			"localisation/reference_l_english.yaml",
			"l_english:\n reference_yaml:0 \"Reference\"\n",
		);
		write_file(compatch.path(), "map/reference.csv", "1;reference\n");
		write_file(
			compatch.path(),
			"pdx_online_assets/reference.json",
			"{\"reference\":true}\n",
		);

		for relative in [
			"music/reference.ogg",
			"sound/reference.wav",
			"gfx/models/reference.mesh",
			"gfx/models/reference.anim",
			"common/reference.bin",
			"common/reference.unknown",
			"music/reference.lua",
		] {
			write_file(compatch.path(), relative, "\0binary");
		}

		assert_eq!(
			reference_output_files(compatch.path()).unwrap(),
			vec![
				"common/defines/00_reference.lua",
				"common/reference.mod",
				"common/scripted_effects/reference.txt",
				"events/reference.TXT",
				"gfx/models/reference.asset",
				"interface/reference.gfx",
				"interface/reference.gui",
				"localisation/reference_l_english.yaml",
				"localisation/reference_l_english.yml",
				"map/reference.csv",
				"pdx_online_assets/reference.json",
			]
		);
	}

	#[test]
	fn reference_output_files_propagates_walk_failures() {
		let root = tempfile::tempdir().unwrap();
		let missing = root.path().join("missing-compatch");
		assert!(reference_output_files(&missing).is_err());
	}

	#[test]
	fn scorer_output_paths_must_belong_to_their_exact_unit() {
		assert!(scoring_evidence_path_belongs_to_unit(
			"interface/reference.gui",
			"interface/reference.gui"
		));
		assert!(!scoring_evidence_path_belongs_to_unit(
			"interface/reference.gui",
			"interface/unrelated.gui"
		));
		assert!(scoring_evidence_path_belongs_to_unit(
			"common/governments/zzz_foch_governments.txt",
			"common/governments/00_governments.txt"
		));
		assert!(!scoring_evidence_path_belongs_to_unit(
			"common/governments/zzz_foch_governments.txt",
			"common/government_names/unrelated.txt"
		));
	}
}
