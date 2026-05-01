use foch_language::analyzer::parser::parse_clausewitz_file;
use std::collections::BTreeMap;
use std::path::PathBuf;
use walkdir::WalkDir;

fn main() {
	let mut args = std::env::args().skip(1);
	let Some(root_arg) = args.next() else {
		eprintln!("usage: cargo run --bin parse_stats -- <root> [--exts txt,gui,gfx]");
		std::process::exit(1);
	};
	let mut exts = vec!["txt".to_string()];
	let mut exclude_prefixes: Vec<String> = Vec::new();

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
		if arg == "--exclude-prefixes"
			&& let Some(value) = args.next()
		{
			exclude_prefixes = value
				.split(',')
				.map(|item| item.trim().trim_matches('/').replace('\\', "/"))
				.filter(|item| !item.is_empty())
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
				"\tline={} col={} msg={}",
				diag.span.start.line, diag.span.start.column, diag.message
			);
		}
		std::process::exit(if parsed.diagnostics.is_empty() { 0 } else { 2 });
	}
	if !root.is_dir() {
		eprintln!("root is not a directory or file: {}", root.display());
		std::process::exit(1);
	}

	let mut files = Vec::new();
	for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let Some(ext) = path.extension() else {
			continue;
		};
		let ext = ext.to_string_lossy();
		let rel = path
			.strip_prefix(&root)
			.unwrap_or(path)
			.to_string_lossy()
			.replace('\\', "/");
		if exclude_prefixes
			.iter()
			.any(|prefix| rel.starts_with(prefix))
		{
			continue;
		}
		if exts
			.iter()
			.any(|candidate| candidate == &ext.to_ascii_lowercase())
		{
			files.push(path.to_path_buf());
		}
	}
	files.sort();

	let mut ok = 0usize;
	let mut failed = 0usize;
	let mut total_diag = 0usize;
	let mut failed_examples: Vec<(PathBuf, usize)> = Vec::new();
	let mut diag_buckets: BTreeMap<String, usize> = BTreeMap::new();

	for file in &files {
		let parsed = parse_clausewitz_file(file);
		if parsed.diagnostics.is_empty() {
			ok += 1;
			continue;
		}

		failed += 1;
		total_diag += parsed.diagnostics.len();
		if failed_examples.len() < 20 {
			failed_examples.push((file.clone(), parsed.diagnostics.len()));
		}

		for diag in &parsed.diagnostics {
			let key = normalize_diag_message(&diag.message);
			*diag_buckets.entry(key).or_insert(0) += 1;
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
		println!("exclude_prefixes={}", exclude_prefixes.join(","));
	}
	println!("total_files={total}");
	println!("ok_files={ok}");
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

fn normalize_diag_message(message: &str) -> String {
	if message.contains("expected '=' after identifier") {
		return "expected '=' after identifier".to_string();
	}
	if message.contains("expected value") {
		return "expected value".to_string();
	}
	if message.contains("expected statement") {
		return "expected statement".to_string();
	}
	if message.contains("unterminated block") {
		return "unterminated block".to_string();
	}
	if message.contains("missing closing brace") {
		return "missing closing brace".to_string();
	}
	"other".to_string()
}
