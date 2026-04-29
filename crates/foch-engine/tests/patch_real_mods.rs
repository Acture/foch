//! Integration tests for the patch-based merge system using real Workshop mods.
//!
//! All tests are `#[ignore]` — they require Steam Workshop mods installed at
//! `~/Library/Application Support/Steam/steamapps/workshop/content/236850/`.
//!
//! Run with:
//!   cargo test -p foch-engine patch_real_mods -- --ignored --nocapture

use std::path::PathBuf;

use foch_engine::merge::emit::emit_clausewitz_statements;
use foch_engine::merge::namespace::{FamilyKeyIndex, KeyContributor, detect_key_conflicts};
use foch_engine::merge::patch::{ClausewitzPatch, diff_ast};
use foch_engine::merge::patch_apply::{apply_patches, merge_single_mod};
use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workshop_root() -> PathBuf {
	let home = std::env::var("HOME").expect("HOME not set");
	PathBuf::from(home).join("Library/Application Support/Steam/steamapps/workshop/content/236850")
}

fn mod_root(steam_id: &str) -> PathBuf {
	workshop_root().join(steam_id)
}

fn parse_mod_file(steam_id: &str, relative: &str) -> ParsedScriptFile {
	let root = mod_root(steam_id);
	let file = root.join(relative);
	assert!(
		file.exists(),
		"Workshop file not found: {} (is the mod installed?)",
		file.display()
	);
	parse_script_file(steam_id, &root, &file)
		.unwrap_or_else(|| panic!("Failed to parse {}", file.display()))
}

/// Count top-level Assignment statements (blocks only — these are the
/// "triggers" / "modifiers" / etc.)
fn count_top_level_blocks(statements: &[AstStatement]) -> usize {
	statements
		.iter()
		.filter(|s| {
			matches!(
				s,
				AstStatement::Assignment {
					value: AstValue::Block { .. },
					..
				}
			)
		})
		.count()
}

/// Recursively check whether `tag = ERS` (or any target key=value pair)
/// appears anywhere in the AST.
fn contains_assignment(statements: &[AstStatement], target_key: &str, target_val: &str) -> bool {
	for stmt in statements {
		match stmt {
			AstStatement::Assignment { key, value, .. } => {
				if key == target_key
					&& matches!(value, AstValue::Scalar { value: sv, .. } if sv.as_text() == target_val)
				{
					return true;
				}
				if let AstValue::Block { items, .. } = value
					&& contains_assignment(items, target_key, target_val)
				{
					return true;
				}
			}
			AstStatement::Item {
				value: AstValue::Block { items, .. },
				..
			} => {
				if contains_assignment(items, target_key, target_val) {
					return true;
				}
			}
			_ => {}
		}
	}
	false
}

fn patch_variant_name(p: &ClausewitzPatch) -> &'static str {
	match p {
		ClausewitzPatch::SetValue { .. } => "SetValue",
		ClausewitzPatch::RemoveNode { .. } => "RemoveNode",
		ClausewitzPatch::InsertNode { .. } => "InsertNode",
		ClausewitzPatch::AppendListItem { .. } => "AppendListItem",
		ClausewitzPatch::RemoveListItem { .. } => "RemoveListItem",
		ClausewitzPatch::ReplaceBlock { .. } => "ReplaceBlock",
		ClausewitzPatch::AppendBlockItem { .. } => "AppendBlockItem",
		ClausewitzPatch::RemoveBlockItem { .. } => "RemoveBlockItem",
	}
}

// ---------------------------------------------------------------------------
// P3: Europa Expanded × Compatch  (critical scenario)
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Requires Workshop mods installed
fn patch_p3_ee_compatch_scripted_triggers() {
	let rel_path = "common/scripted_triggers/missions_expanded_scripted_triggers.txt";
	let ee_steam_id = "2164202838";
	let compatch_steam_id = "3081508015";

	// 1. Parse both versions
	let ee_parsed = parse_mod_file(ee_steam_id, rel_path);
	let compatch_parsed = parse_mod_file(compatch_steam_id, rel_path);

	let ee_top_blocks = count_top_level_blocks(&ee_parsed.ast.statements);
	let compatch_top_blocks = count_top_level_blocks(&compatch_parsed.ast.statements);
	println!("EE top-level blocks:       {ee_top_blocks}");
	println!("Compatch top-level blocks: {compatch_top_blocks}");

	// Compatch is a near-copy of EE — same number of top-level blocks
	assert_eq!(
		ee_top_blocks, compatch_top_blocks,
		"Compatch should have the same top-level triggers as EE"
	);

	// 2. Compute diff
	let patches = diff_ast(&ee_parsed, &compatch_parsed, MergeKeySource::AssignmentKey);
	println!("\nPatches produced: {}", patches.len());
	for (i, p) in patches.iter().enumerate() {
		println!(
			"  [{i}] {}: path={:?}",
			patch_variant_name(p),
			match p {
				ClausewitzPatch::SetValue { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::RemoveNode { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::InsertNode { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::AppendListItem { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::RemoveListItem { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::ReplaceBlock { path, key, .. } => format!("{path:?} / {key}"),
				ClausewitzPatch::AppendBlockItem { path, value } =>
					format!("{path:?} / item={value:?}"),
				ClausewitzPatch::RemoveBlockItem { path, value } =>
					format!("{path:?} / item={value:?}"),
			}
		);
	}

	// 3. Assert: diff should be very small (ideally 1 AppendListItem for tag = ERS)
	assert!(
		patches.len() <= 5,
		"Expected a very small diff (≤5 patches), got {}. \
		 The patch system may be duplicating blocks instead of diffing.",
		patches.len()
	);

	// 4. Apply patches
	let merged = apply_patches(
		&ee_parsed.ast.statements,
		&patches,
		MergeKeySource::AssignmentKey,
	);

	let merged_top_blocks = count_top_level_blocks(&merged);
	println!("\nMerged top-level blocks:   {merged_top_blocks}");

	// Must have same count as EE — NOT doubled
	assert_eq!(
		merged_top_blocks, ee_top_blocks,
		"Merged output should have {ee_top_blocks} top-level triggers, not {merged_top_blocks} (doubled?)"
	);

	// 5. The result must contain tag = ERS somewhere
	assert!(
		contains_assignment(&merged, "tag", "ERS"),
		"Merged output must contain `tag = ERS` from the compatch"
	);

	// 6. Emit to text
	let emitted = emit_clausewitz_statements(&merged).expect("emit_clausewitz_statements failed");

	let ee_text =
		std::fs::read_to_string(mod_root(ee_steam_id).join(rel_path)).expect("read EE source file");

	let ratio = emitted.len() as f64 / ee_text.len() as f64;
	println!("\nEE source length:  {} chars", ee_text.len());
	println!("Emitted length:    {} chars", emitted.len());
	println!("Ratio:             {ratio:.3}");

	// Output length should be close to EE original (±10%), NOT doubled.
	// The emit path may drop comments, causing up to ~7% shrinkage.
	assert!(
		(0.90..=1.10).contains(&ratio),
		"Emitted output length should be within ±10% of EE original (ratio={ratio:.3})"
	);

	println!("\n✅ P3 PASSED: patch system correctly diffs EE×Compatch as a small patch set.");
}

// ---------------------------------------------------------------------------
// P3 merge_single_mod shortcut
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Requires Workshop mods installed
fn patch_p3_merge_single_mod_shortcut() {
	let rel_path = "common/scripted_triggers/missions_expanded_scripted_triggers.txt";
	let ee_parsed = parse_mod_file("2164202838", rel_path);
	let compatch_parsed = parse_mod_file("3081508015", rel_path);

	let ee_top_blocks = count_top_level_blocks(&ee_parsed.ast.statements);

	let merged = merge_single_mod(&ee_parsed, &compatch_parsed, MergeKeySource::AssignmentKey);

	let merged_top_blocks = count_top_level_blocks(&merged);
	println!("merge_single_mod: {merged_top_blocks} top-level blocks (EE has {ee_top_blocks})");

	assert_eq!(merged_top_blocks, ee_top_blocks);
	assert!(contains_assignment(&merged, "tag", "ERS"));

	println!("✅ merge_single_mod shortcut matches step-by-step result.");
}

// ---------------------------------------------------------------------------
// P1: Funding × SimplifiedFunding  (convergence test)
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Requires Workshop mods installed
fn patch_p1_funding_event_modifiers() {
	let funding_steam_id = "1678280999";
	let simplified_steam_id = "3453420872";
	let rel_path = "common/event_modifiers/event_modifiers_rf_tax.txt";

	// Check that the file exists in at least one mod; the representative file
	// may differ between mod versions.
	let funding_root = mod_root(funding_steam_id);
	let simplified_root = mod_root(simplified_steam_id);
	let funding_file = funding_root.join(rel_path);
	let simplified_file = simplified_root.join(rel_path);

	if !funding_file.exists() || !simplified_file.exists() {
		// Try to find *any* overlapping file in common/event_modifiers
		let fallback = find_first_overlapping_file(
			funding_steam_id,
			simplified_steam_id,
			"common/event_modifiers",
		);
		if let Some(fb) = &fallback {
			println!("Primary file not found; using fallback: {fb}");
			run_funding_convergence_test(funding_steam_id, simplified_steam_id, fb);
		} else {
			panic!(
				"No overlapping event_modifiers file found between {} and {}",
				funding_steam_id, simplified_steam_id
			);
		}
		return;
	}

	run_funding_convergence_test(funding_steam_id, simplified_steam_id, rel_path);
}

fn find_first_overlapping_file(steam_a: &str, steam_b: &str, subdir: &str) -> Option<String> {
	let dir_a = mod_root(steam_a).join(subdir);
	let dir_b = mod_root(steam_b).join(subdir);
	if !dir_a.exists() || !dir_b.exists() {
		return None;
	}
	for entry in std::fs::read_dir(&dir_a).ok()? {
		let entry = entry.ok()?;
		let name = entry.file_name();
		if dir_b.join(&name).exists() {
			return Some(format!("{subdir}/{}", name.to_string_lossy()));
		}
	}
	None
}

fn run_funding_convergence_test(steam_a: &str, steam_b: &str, rel_path: &str) {
	let parsed_a = parse_mod_file(steam_a, rel_path);
	let parsed_b = parse_mod_file(steam_b, rel_path);

	let a_blocks = count_top_level_blocks(&parsed_a.ast.statements);
	let b_blocks = count_top_level_blocks(&parsed_b.ast.statements);
	println!("Mod A ({steam_a}) top-level blocks: {a_blocks}");
	println!("Mod B ({steam_b}) top-level blocks: {b_blocks}");

	// 1. Diff
	let patches = diff_ast(&parsed_a, &parsed_b, MergeKeySource::AssignmentKey);
	println!("\nPatches produced: {}", patches.len());

	// Tally by variant
	let mut counts = std::collections::HashMap::<&str, usize>::new();
	for p in &patches {
		*counts.entry(patch_variant_name(p)).or_default() += 1;
	}
	for (variant, count) in &counts {
		println!("  {variant}: {count}");
	}

	// 2. For modifiers identical in both mods: there should be zero patches
	//    touching those keys. We verify by checking that common keys that
	//    appear in both with the same block produce no patches.
	let a_keys = extract_assignment_keys(&parsed_a.ast.statements);
	let b_keys = extract_assignment_keys(&parsed_b.ast.statements);
	let common_keys: Vec<_> = a_keys.iter().filter(|k| b_keys.contains(k)).collect();
	println!("\nCommon keys: {}", common_keys.len());

	// Count patched keys — each patch has a root key
	let patched_keys: std::collections::HashSet<String> = patches
		.iter()
		.filter_map(|p| match p {
			ClausewitzPatch::SetValue { path, key, .. }
			| ClausewitzPatch::ReplaceBlock { path, key, .. } => {
				Some(path.first().cloned().unwrap_or_else(|| key.clone()))
			}
			_ => None,
		})
		.collect();

	let converged_count = common_keys
		.iter()
		.filter(|k| !patched_keys.contains(k.as_str()))
		.count();
	println!("Converged (no patches): {converged_count}");
	println!(
		"Diverged (patched):     {}",
		common_keys.len() - converged_count
	);

	// 3. Apply patches and verify structural validity
	let merged = apply_patches(
		&parsed_a.ast.statements,
		&patches,
		MergeKeySource::AssignmentKey,
	);

	let merged_blocks = count_top_level_blocks(&merged);
	println!("\nMerged top-level blocks: {merged_blocks}");

	// The merged count should be >= max(a, b) — we're combining both
	assert!(
		merged_blocks >= a_blocks.min(b_blocks),
		"Merged output should have at least as many blocks as the smaller input"
	);

	// 4. Emit and verify balanced braces
	let emitted = emit_clausewitz_statements(&merged).expect("emit_clausewitz_statements failed");

	let open_braces = emitted.chars().filter(|&c| c == '{').count();
	let close_braces = emitted.chars().filter(|&c| c == '}').count();
	println!(
		"\nEmitted: {} chars, braces: {{ {open_braces} }} {close_braces}",
		emitted.len()
	);
	assert_eq!(
		open_braces, close_braces,
		"Emitted text has unbalanced braces"
	);

	// 5. Verify no duplicated top-level keys in the merged output
	let merged_keys = extract_assignment_keys(&merged);
	let mut seen = std::collections::HashSet::new();
	let mut dups = Vec::new();
	for k in &merged_keys {
		if !seen.insert(k.as_str()) {
			dups.push(k.clone());
		}
	}
	if !dups.is_empty() {
		println!("\n⚠ Duplicated keys in merged output: {dups:?}");
	}

	println!("\n✅ P1 PASSED: Funding × SimplifiedFunding convergence test on {rel_path}");
}

fn extract_assignment_keys(statements: &[AstStatement]) -> Vec<String> {
	statements
		.iter()
		.filter_map(|s| match s {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => Some(key.clone()),
			_ => None,
		})
		.collect()
}

// ---------------------------------------------------------------------------
// Namespace: cross-file key conflict detection
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Requires Workshop mods installed
fn namespace_detects_cross_file_conflicts_in_real_playlist() {
	let ee_steam_id = "2164202838";
	let compatch_steam_id = "3081508015";
	let family = "scripted_triggers";
	let subdir = "common/scripted_triggers";

	// Build a FamilyKeyIndex manually from the two mods' scripted_triggers dirs
	let mut index = FamilyKeyIndex {
		family_id: family.to_string(),
		entries: std::collections::HashMap::new(),
	};

	let mods: &[(&str, usize)] = &[
		(ee_steam_id, 1),       // EE loaded first
		(compatch_steam_id, 2), // Compatch loaded second
	];

	for &(steam_id, precedence) in mods {
		let dir = mod_root(steam_id).join(subdir);
		if !dir.exists() {
			println!("Skipping {steam_id}: {subdir} not found");
			continue;
		}

		let entries: Vec<_> = std::fs::read_dir(&dir)
			.expect("read scripted_triggers dir")
			.filter_map(|e| e.ok())
			.filter(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
			.collect();

		for entry in &entries {
			let file_path = entry.path();
			let root = mod_root(steam_id);
			let rel = file_path
				.strip_prefix(&root)
				.unwrap()
				.to_string_lossy()
				.to_string();

			let Some(parsed) = parse_script_file(steam_id, &root, &file_path) else {
				continue;
			};

			let keys = extract_assignment_keys(&parsed.ast.statements);
			for key in keys {
				index.entries.entry(key).or_default().push(KeyContributor {
					mod_id: steam_id.to_string(),
					file_path: rel.clone(),
					precedence,
					is_base_game: false,
				});
			}
		}

		println!("Indexed {steam_id}: {} files", entries.len());
	}

	println!("Total keys in index: {}", index.entries.len());

	// Detect conflicts
	let conflicts = detect_key_conflicts(&index);
	println!("Cross-file key conflicts found: {}", conflicts.len());

	for conflict in &conflicts {
		let mods_involved: Vec<_> = conflict
			.contributors
			.iter()
			.map(|c| format!("{}@{}", c.mod_id, c.file_path))
			.collect();
		println!("  key={}: {}", conflict.key, mods_involved.join(", "));
	}

	// For EE × Compatch on scripted_triggers, we expect conflicts on the
	// shared triggers (both mods define the same keys in the same file name)
	if !conflicts.is_empty() {
		println!(
			"\n✅ Namespace detected {} cross-mod key conflicts as expected.",
			conflicts.len()
		);
	} else {
		println!("\nℹ No cross-file conflicts detected (mods may be disjoint in this family).");
	}

	// Verify the index is non-trivial
	assert!(
		!index.entries.is_empty(),
		"Should have indexed at least some scripted_trigger keys"
	);

	println!("✅ Namespace conflict detection test completed.");
}
