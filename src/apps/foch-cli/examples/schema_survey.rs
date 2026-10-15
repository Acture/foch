//! Runs the editor's schema diagnostics over every script under a game or mod
//! root and groups what they report. On the unmodified game every finding is
//! a false positive or a genuine fault in the game's own files, so a run
//! before and after a checker change measures that change.
//!
//! For a mod, `--base <game root>` adds the game's scripts to the workspace
//! that names definitions, with a mod file replacing the game file at the same
//! path, while only the mod's own files are checked. Other mod loading rules,
//! such as `replace_path`, are not modelled.

use foch::game::eu4::editor::schema::{EditorSchema, SchemaDocument};
use foch::game::eu4::text::decode_paradox_bytes;
use foch::model::GamePathBuf;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const USAGE: &str = "usage: cargo run -p foch-cli --release --example schema_survey --features dev-tools -- <root> [--base <game root>] [--prefix <game path>] [--tsv <out.tsv>] [--baseline <earlier.tsv>] [--top <n>]";

struct Options {
	root: PathBuf,
	base: Option<PathBuf>,
	prefix: Option<GamePathBuf>,
	tsv: Option<PathBuf>,
	baseline: Option<PathBuf>,
	top: usize,
}

struct Finding {
	path: String,
	line: u32,
	code: String,
	severity: String,
	message: String,
}

fn main() {
	let options = parse_options();
	let files = load_files(&options.root);
	let base_files = options.base.as_deref().map(load_files).unwrap_or_default();
	let schema = EditorSchema::active();
	let documents = base_files
		.iter()
		.filter(|(path, _)| {
			files
				.binary_search_by(|(file_path, _)| file_path.cmp(path))
				.is_err()
		})
		.chain(&files)
		.map(|(path, text)| SchemaDocument::new(path, text))
		.collect::<Vec<_>>();
	let workspace = schema.workspace(&documents);
	let checked = files
		.iter()
		.filter(|(path, _)| {
			options
				.prefix
				.as_ref()
				.is_none_or(|prefix| path.starts_with(prefix))
		})
		.collect::<Vec<_>>();
	let findings = survey(&schema, &workspace, &checked);

	if let Some(tsv) = &options.tsv {
		write_tsv(tsv, &findings);
	}
	println!("root={}", options.root.display());
	if let Some(base) = &options.base {
		println!("base={}", base.display());
	}
	if let Some(prefix) = &options.prefix {
		println!("prefix={}", prefix.as_str());
	}
	println!("workspace_files={}", documents.len());
	println!("checked_files={}", checked.len());
	println!("diagnostics={}", findings.len());

	let mut by_code = BTreeMap::<&str, usize>::new();
	for finding in &findings {
		*by_code.entry(finding.code.as_str()).or_default() += 1;
	}
	println!("by_code:");
	for (code, count) in &by_code {
		println!("\t{code}\t{count}");
	}

	let groups = group(findings.iter().map(|finding| {
		(
			finding.code.as_str(),
			finding.severity.as_str(),
			finding.path.as_str(),
			finding.message.as_str(),
		)
	}));
	println!("top_groups (count, code, severity, folder, message):");
	let mut ranked = groups.iter().collect::<Vec<_>>();
	ranked.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
	for (key, count) in ranked.into_iter().take(options.top) {
		println!("\t{count}\t{key}");
	}

	if let Some(baseline) = &options.baseline {
		print_baseline_delta(baseline, &groups, options.top);
	}
}

fn parse_options() -> Options {
	let mut args = std::env::args().skip(1);
	let Some(root) = args.next().filter(|arg| !arg.starts_with("--")) else {
		fail(USAGE);
	};
	let mut options = Options {
		root: PathBuf::from(root),
		base: None,
		prefix: None,
		tsv: None,
		baseline: None,
		top: 40,
	};
	while let Some(arg) = args.next() {
		let Some(value) = args.next() else {
			fail(&format!("{arg} needs a value\n{USAGE}"));
		};
		match arg.as_str() {
			"--base" => options.base = Some(PathBuf::from(value)),
			"--prefix" => {
				options.prefix = Some(GamePathBuf::parse(&value).unwrap_or_else(|error| {
					fail(&format!("invalid --prefix: {error}"));
				}));
			}
			"--tsv" => options.tsv = Some(PathBuf::from(value)),
			"--baseline" => options.baseline = Some(PathBuf::from(value)),
			"--top" => {
				options.top = value.parse().unwrap_or_else(|error| {
					fail(&format!("invalid --top: {error}"));
				});
			}
			_ => fail(&format!("unknown option {arg}\n{USAGE}")),
		}
	}
	for dir in std::iter::once(&options.root).chain(&options.base) {
		if !dir.is_dir() {
			fail(&format!("not a directory: {}", dir.display()));
		}
	}
	options
}

fn fail(message: &str) -> ! {
	eprintln!("{message}");
	std::process::exit(1);
}

/// Every `.txt` script under the root, by game path. The whole root is the
/// workspace that names definitions, even when `--prefix` narrows the files
/// checked.
fn load_files(root: &Path) -> Vec<(GamePathBuf, String)> {
	let mut files = Vec::new();
	for entry in WalkDir::new(root) {
		let entry = entry.unwrap_or_else(|error| fail(&format!("walk failed: {error}")));
		let path = entry.path();
		if !entry.file_type().is_file()
			|| !path
				.extension()
				.and_then(|extension| extension.to_str())
				.is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
		{
			continue;
		}
		let game_path = GamePathBuf::from_physical(root, path).unwrap_or_else(|error| {
			fail(&format!("{} has no game path: {error}", path.display()));
		});
		let bytes = fs::read(path)
			.unwrap_or_else(|error| fail(&format!("read {} failed: {error}", path.display())));
		files.push((game_path, decode_paradox_bytes(&bytes).into_owned()));
	}
	files.sort_by(|left, right| left.0.cmp(&right.0));
	files
}

fn survey(
	schema: &EditorSchema,
	workspace: &foch::game::eu4::editor::schema::SchemaWorkspace,
	files: &[&(GamePathBuf, String)],
) -> Vec<Finding> {
	let threads = std::thread::available_parallelism().map_or(1, usize::from);
	let chunk_size = files.len().div_ceil(threads).max(1);
	std::thread::scope(|scope| {
		let workers = files
			.chunks(chunk_size)
			.map(|chunk| {
				scope.spawn(move || {
					chunk
						.iter()
						.flat_map(|(path, text)| {
							schema
								.diagnostics(path, text, Some(workspace))
								.into_iter()
								.map(move |diagnostic| Finding {
									path: path.as_str().to_string(),
									line: diagnostic.range.start.line + 1,
									code: diagnostic.code.unwrap_or_default(),
									severity: diagnostic
										.severity
										.map(|severity| format!("{severity:?}"))
										.unwrap_or_default(),
									message: diagnostic.message.replace(['\t', '\n'], " "),
								})
						})
						.collect::<Vec<_>>()
				})
			})
			.collect::<Vec<_>>();
		workers
			.into_iter()
			.flat_map(|worker| worker.join().expect("survey worker panicked"))
			.collect()
	})
}

fn write_tsv(path: &Path, findings: &[Finding]) {
	let file = fs::File::create(path)
		.unwrap_or_else(|error| fail(&format!("create {} failed: {error}", path.display())));
	let mut out = BufWriter::new(file);
	for finding in findings {
		writeln!(
			out,
			"{}\t{}\t{}\t{}\t{}",
			finding.path, finding.line, finding.code, finding.severity, finding.message
		)
		.unwrap_or_else(|error| fail(&format!("write {} failed: {error}", path.display())));
	}
	out.flush()
		.unwrap_or_else(|error| fail(&format!("write {} failed: {error}", path.display())));
}

/// Counts findings by code, severity, content folder and message, with the
/// quoted names in the message kept and any list of allowed values dropped.
fn group<'a>(
	findings: impl Iterator<Item = (&'a str, &'a str, &'a str, &'a str)>,
) -> BTreeMap<String, usize> {
	let mut groups = BTreeMap::new();
	for (code, severity, path, message) in findings {
		let message = message
			.split_once(" (allowed:")
			.map_or(message, |(head, _)| head);
		*groups
			.entry(format!("{code}\t{severity}\t{}\t{message}", folder(path)))
			.or_default() += 1;
	}
	groups
}

/// `common/<family>` and `history/<family>` name a content family; elsewhere
/// the top directory does.
fn folder(path: &str) -> &str {
	let mut components = path.match_indices('/');
	let first = components.next().map(|(index, _)| index);
	let second = components.next().map(|(index, _)| index);
	match (first, second) {
		(Some(first), Some(second)) if matches!(&path[..first], "common" | "history") => {
			&path[..second]
		}
		(Some(first), _) => &path[..first],
		_ => path,
	}
}

fn print_baseline_delta(baseline: &Path, current: &BTreeMap<String, usize>, top: usize) {
	let text = fs::read_to_string(baseline)
		.unwrap_or_else(|error| fail(&format!("read {} failed: {error}", baseline.display())));
	let rows = text
		.lines()
		.filter_map(|line| {
			let fields = line.splitn(5, '\t').collect::<Vec<_>>();
			(fields.len() == 5).then(|| (fields[2], fields[3], fields[0], fields[4]))
		})
		.collect::<Vec<_>>();
	let earlier = group(rows.iter().copied());
	println!(
		"baseline={} diagnostics={} delta={}",
		baseline.display(),
		rows.len(),
		current.values().sum::<usize>() as i64 - rows.len() as i64
	);
	let mut deltas = earlier
		.keys()
		.chain(current.keys())
		.collect::<std::collections::BTreeSet<_>>()
		.into_iter()
		.filter_map(|key| {
			let before = earlier.get(key).copied().unwrap_or_default() as i64;
			let after = current.get(key).copied().unwrap_or_default() as i64;
			(before != after).then_some((after - before, key))
		})
		.collect::<Vec<_>>();
	deltas.sort_by(|left, right| {
		right
			.0
			.abs()
			.cmp(&left.0.abs())
			.then_with(|| left.1.cmp(right.1))
	});
	println!("changed_groups (delta, code, severity, folder, message):");
	for (delta, key) in deltas.into_iter().take(top) {
		println!("\t{delta:+}\t{key}");
	}
}
