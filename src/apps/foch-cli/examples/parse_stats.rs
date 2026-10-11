use foch::game::eu4::script::parse_cache::parse_clausewitz_for_path;
use foch::game::eu4::script::parser::{ParsedStatements, parse_clausewitz_file};
use foch::game::eu4::text::decode_paradox_bytes;
use foch::model::GamePathBuf;
use std::collections::BTreeMap;
use std::path::PathBuf;
use walkdir::WalkDir;

fn main() {
	let mut args = std::env::args().skip(1);
	let Some(root_arg) = args.next() else {
		eprintln!(
			"usage: cargo run -p foch-cli --example parse_stats --features dev-tools -- <root> [--exts txt,gui,gfx] [--mod-roots]"
		);
		std::process::exit(1);
	};
	let mut exts = vec!["txt".to_string()];
	let mut exclude_prefixes: Vec<GamePathBuf> = Vec::new();
	// Each directory directly under the root is a mod, and a file is read as
	// the game path it has in its mod, under that path's schema.
	let mut mod_roots = false;

	while let Some(arg) = args.next() {
		if arg == "--exts"
			&& let Some(value) = args.next()
		{
			exts = value
				.split(',')
				.map(|item| item.trim().to_ascii_lowercase())
				.filter(|item| !item.is_empty())
				.collect();
		}
		if arg == "--mod-roots" {
			mod_roots = true;
		}
		// Prefixes are game paths in `/` syntax, compared by whole components.
		if arg == "--exclude-prefixes"
			&& let Some(value) = args.next()
		{
			exclude_prefixes = value
				.split(',')
				.map(str::trim)
				.filter(|item| !item.is_empty())
				.map(|item| {
					GamePathBuf::parse(item).unwrap_or_else(|error| {
						eprintln!("invalid --exclude-prefixes entry: {error}");
						std::process::exit(1);
					})
				})
				.collect();
		}
	}

	let root = PathBuf::from(root_arg);
	if root.is_file() {
		let parsed = parse_clausewitz_file(&root);
		println!("file={}", root.display());
		println!("diagnostics={}", parsed.diagnostics.len());
		for diag in parsed.diagnostics.iter().take(40) {
			println!(
				"\tline={} col={} code={:?} msg={}{}",
				diag.span.start.line,
				diag.span.start.column,
				diag.code,
				diag.message,
				diag.repair
					.map(|repair| format!(" repair={}", repair.description()))
					.unwrap_or_default()
			);
		}
		let fatal = parsed.diagnostics.iter().any(|diag| diag.repair.is_none());
		std::process::exit(if fatal { 2 } else { 0 });
	}
	if !root.is_dir() {
		eprintln!("root is not a directory or file: {}", root.display());
		std::process::exit(1);
	}

	let mut files = Vec::new();
	for entry in WalkDir::new(&root) {
		let entry = entry.unwrap_or_else(|error| {
			eprintln!("walk failed: {error}");
			std::process::exit(1);
		});
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
			continue;
		};
		if !exclude_prefixes.is_empty() {
			let relative = GamePathBuf::from_physical(&root, path).unwrap_or_else(|error| {
				eprintln!("{} has no game path: {error}", path.display());
				std::process::exit(1);
			});
			if exclude_prefixes
				.iter()
				.any(|prefix| relative.starts_with(prefix))
			{
				continue;
			}
		}
		if exts
			.iter()
			.any(|candidate| candidate.eq_ignore_ascii_case(ext))
		{
			files.push(path.to_path_buf());
		}
	}
	files.sort();

	let mut ok = 0usize;
	let mut repaired = 0usize;
	let mut isolating = 0usize;
	let mut isolations: Vec<String> = Vec::new();
	let mut failed = 0usize;
	let mut total_diag = 0usize;
	let mut failed_examples: Vec<(PathBuf, usize)> = Vec::new();
	let mut repairs: Vec<String> = Vec::new();
	let mut diag_buckets: BTreeMap<String, usize> = BTreeMap::new();

	for file in &files {
		let parsed = if mod_roots {
			parse_in_mod(&root, file)
		} else {
			parse_clausewitz_file(file)
		};
		if parsed.diagnostics.is_empty() {
			ok += 1;
			continue;
		}

		total_diag += parsed.diagnostics.len();
		for diag in &parsed.diagnostics {
			let state = if diag.repair.is_some() {
				"repaired"
			} else if diag.isolation.is_some() {
				"isolated"
			} else {
				"fatal"
			};
			*diag_buckets
				.entry(format!("{:?} ({state})", diag.code))
				.or_insert(0) += 1;
		}
		let rel = file.strip_prefix(&root).unwrap_or(file.as_path());
		if parsed
			.diagnostics
			.iter()
			.all(|diag| diag.repair.is_some() || diag.isolation.is_some())
			&& parsed
				.diagnostics
				.iter()
				.any(|diag| diag.isolation.is_some())
		{
			isolating += 1;
			for diag in &parsed.diagnostics {
				if let Some(isolation) = &diag.isolation {
					isolations.push(format!(
						"{}:{}-{} `{}` proposals: {}",
						rel.display(),
						diag.span.start.line,
						isolation.end_line,
						isolation.definition,
						isolation
							.proposals
							.iter()
							.map(|proposal| proposal.description())
							.collect::<Vec<_>>()
							.join(", ")
					));
				}
			}
			continue;
		}
		if parsed.diagnostics.iter().all(|diag| diag.repair.is_some()) {
			repaired += 1;
			let rel = file.strip_prefix(&root).unwrap_or(file.as_path());
			for diag in &parsed.diagnostics {
				if let Some(repair) = diag.repair {
					repairs.push(format!(
						"{}:{}:{} {}",
						rel.display(),
						diag.span.start.line,
						diag.span.start.column,
						repair.description()
					));
				}
			}
			continue;
		}

		failed += 1;
		if failed_examples.len() < 20 {
			failed_examples.push((file.clone(), parsed.diagnostics.len()));
		}
	}

	let total = files.len();
	let rate = if total == 0 {
		0.0
	} else {
		(ok as f64) * 100.0 / (total as f64)
	};

	println!("root={}", root.display());
	println!("extensions={}", exts.join(","));
	if !exclude_prefixes.is_empty() {
		let prefixes = exclude_prefixes
			.iter()
			.map(|prefix| prefix.as_str())
			.collect::<Vec<_>>();
		println!("exclude_prefixes={}", prefixes.join(","));
	}
	println!("total_files={total}");
	println!("ok_files={ok}");
	println!("repaired_files={repaired}");
	println!("isolating_files={isolating}");
	println!("failed_files={failed}");
	println!("success_rate_percent={rate:.4}");
	println!("total_diagnostics={total_diag}");

	if !diag_buckets.is_empty() {
		println!("top_diagnostic_categories:");
		let mut entries: Vec<(&str, usize)> = diag_buckets
			.iter()
			.map(|(kind, count)| (kind.as_str(), *count))
			.collect();
		entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
		for (idx, (kind, count)) in entries.into_iter().take(10).enumerate() {
			let rank = idx + 1;
			println!("\t{rank}. {kind}: {count}");
		}
	}

	if !repairs.is_empty() {
		println!("repairs:");
		for repair in &repairs {
			println!("\t{repair}");
		}
	}

	if !isolations.is_empty() {
		println!("isolations:");
		for isolation in &isolations {
			println!("\t{isolation}");
		}
	}

	if !failed_examples.is_empty() {
		println!("failed_examples:");
		for (path, count) in failed_examples {
			let rel = path.strip_prefix(&root).unwrap_or(path.as_path());
			println!("\t{} (diagnostics={count})", rel.display());
		}
	}

	if failed > 0 {
		std::process::exit(2);
	}
}

/// Parses `file` as the game path it has under the mod directory that holds
/// it, directly under `root`.
fn parse_in_mod(root: &std::path::Path, file: &std::path::Path) -> ParsedStatements {
	let relative = file.strip_prefix(root).expect("file under the root");
	let mod_dir = relative
		.components()
		.next()
		.expect("file inside a mod directory");
	let mod_root = root.join(mod_dir);
	let game_path = GamePathBuf::from_physical(&mod_root, file).unwrap_or_else(|error| {
		eprintln!("{} has no game path: {error}", file.display());
		std::process::exit(1);
	});
	let bytes = std::fs::read(file).unwrap_or_else(|error| {
		eprintln!("failed to read {}: {error}", file.display());
		std::process::exit(1);
	});
	let parsed = parse_clausewitz_for_path(&game_path, &decode_paradox_bytes(&bytes));
	ParsedStatements {
		statements: parsed.ast.statements,
		diagnostics: parsed.diagnostics,
	}
}
