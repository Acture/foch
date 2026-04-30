use super::semantic_index::count_symbol_references_resolving_to_mod;
use foch_core::model::{
	CheckContext, DepMisuseEvidence, DepMisuseFinding, Finding, FindingChannel, Severity,
	SymbolKind,
};
use std::collections::{HashMap, HashSet};

pub fn check_required_fields(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();

	for (idx, entry) in ctx.playlist.mods.iter().enumerate() {
		let mod_id = entry.steam_id.clone();

		if entry
			.display_name
			.as_deref()
			.map(str::trim)
			.unwrap_or("")
			.is_empty()
		{
			findings.push(new_finding(
				"R002",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod 条目 {idx} 缺失 displayName"),
				mod_id.clone(),
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}

		if entry
			.steam_id
			.as_deref()
			.map(str::trim)
			.unwrap_or("")
			.is_empty()
		{
			findings.push(new_finding(
				"R002",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod 条目 {idx} 缺失 steamId"),
				None,
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}

		if entry.position.is_none() {
			findings.push(new_finding(
				"R002",
				Severity::Error,
				FindingChannel::Strict,
				format!("mod 条目 {idx} 缺失 position"),
				mod_id,
				Some(ctx.playlist_path.clone()),
				None,
				None,
				None,
				Some(1.0),
			));
		}
	}

	findings
}

pub fn check_duplicate_mod_identity(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut seen_steam_ids = HashMap::<String, usize>::new();
	let mut seen_positions = HashMap::<usize, usize>::new();

	for (idx, entry) in ctx.playlist.mods.iter().enumerate() {
		if let Some(steam_id) = entry.steam_id.as_ref() {
			if let Some(first_idx) = seen_steam_ids.get(steam_id) {
				findings.push(new_finding(
					"R003",
					Severity::Error,
					FindingChannel::Strict,
					format!("steamId 冲突: {steam_id} (首次出现于索引 {first_idx})"),
					Some(steam_id.clone()),
					Some(ctx.playlist_path.clone()),
					Some(format!("重复条目索引: {idx}")),
					None,
					None,
					Some(1.0),
				));
			} else {
				seen_steam_ids.insert(steam_id.clone(), idx);
			}
		}

		if let Some(position) = entry.position {
			if let Some(first_idx) = seen_positions.get(&position) {
				findings.push(new_finding(
					"R003",
					Severity::Error,
					FindingChannel::Strict,
					format!("position 冲突: {position} (首次出现于索引 {first_idx})"),
					entry.steam_id.clone(),
					Some(ctx.playlist_path.clone()),
					Some(format!("重复条目索引: {idx}")),
					None,
					None,
					Some(1.0),
				));
			} else {
				seen_positions.insert(position, idx);
			}
		}
	}

	findings
}

pub fn check_missing_descriptor(ctx: &CheckContext) -> Vec<Finding> {
	let mut findings = Vec::new();

	for mod_item in &ctx.mods {
		if !mod_item.entry.enabled {
			continue;
		}

		match (
			&mod_item.descriptor_path,
			&mod_item.descriptor,
			&mod_item.descriptor_error,
		) {
			(None, _, _) => findings.push(new_finding(
				"R004",
				Severity::Error,
				FindingChannel::Strict,
				"无法定位 descriptor.mod".to_string(),
				Some(mod_item.mod_id.clone()),
				mod_item.root_path.clone(),
				None,
				None,
				None,
				Some(1.0),
			)),
			(Some(path), None, Some(err)) => findings.push(new_finding(
				"R004",
				Severity::Error,
				FindingChannel::Strict,
				"descriptor.mod 解析失败".to_string(),
				Some(mod_item.mod_id.clone()),
				Some(path.clone()),
				Some(err.clone()),
				None,
				None,
				Some(1.0),
			)),
			(Some(path), None, None) => findings.push(new_finding(
				"R004",
				Severity::Error,
				FindingChannel::Strict,
				"descriptor.mod 不存在".to_string(),
				Some(mod_item.mod_id.clone()),
				Some(path.clone()),
				None,
				None,
				None,
				Some(1.0),
			)),
			_ => {}
		}
	}

	findings
}

pub fn check_file_conflict(ctx: &CheckContext) -> Vec<Finding> {
	let mut file_owners: HashMap<String, Vec<String>> = HashMap::new();
	for mod_item in &ctx.mods {
		for file in &mod_item.files {
			let key = file.to_string_lossy().to_string();
			file_owners
				.entry(key)
				.or_default()
				.push(mod_item.mod_id.clone());
		}
	}

	let mut findings = Vec::new();
	for (path, owners) in file_owners {
		let unique: Vec<String> = {
			let mut seen = HashSet::new();
			owners
				.into_iter()
				.filter(|owner| seen.insert(owner.clone()))
				.collect()
		};

		if unique.len() < 2 {
			continue;
		}

		let mergeable = is_structurally_mergeable_path(&path);
		let message = if mergeable {
			format!("文件覆盖冲突（可结构化自动合并候选）: {path}")
		} else {
			format!("文件覆盖冲突: {path}")
		};
		let evidence = if mergeable {
			format!("{} | merge_hint=structural", unique.join(" -> "))
		} else {
			unique.join(" -> ")
		};

		findings.push(new_finding(
			"R005",
			Severity::Warning,
			FindingChannel::Advisory,
			message,
			unique.last().cloned(),
			Some(path.into()),
			Some(evidence),
			None,
			None,
			Some(if mergeable { 0.75 } else { 0.85 }),
		));
	}

	findings
}

fn is_structurally_mergeable_path(path: &str) -> bool {
	let normalized = path.replace('\\', "/").to_ascii_lowercase();
	if normalized.ends_with(".gui") || normalized.ends_with(".gfx") {
		return normalized.starts_with("interface/")
			|| normalized.starts_with("common/interface/")
			|| normalized.starts_with("gfx/");
	}
	if normalized.ends_with(".txt") || normalized.ends_with(".lua") {
		return normalized.starts_with("events/")
			|| normalized.starts_with("decisions/")
			|| normalized.starts_with("common/scripted_effects/")
			|| normalized.starts_with("common/diplomatic_actions/")
			|| normalized.starts_with("common/triggered_modifiers/")
			|| normalized.starts_with("common/defines/")
			|| normalized.starts_with("interface/")
			|| normalized.starts_with("common/interface/");
	}
	false
}

pub fn check_missing_dependency(ctx: &CheckContext) -> Vec<Finding> {
	let identity = foch_core::domain::dep_resolution::ModIdentityIndex::from_mods(&ctx.mods);

	let mut findings = Vec::new();
	for mod_item in &ctx.mods {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			if identity.contains(dependency) {
				continue;
			}

			findings.push(new_finding(
				"R006",
				Severity::Warning,
				FindingChannel::Advisory,
				format!("缺失依赖: {dependency}"),
				Some(mod_item.mod_id.clone()),
				mod_item.descriptor_path.clone(),
				Some(format!("{} 依赖 {dependency}", descriptor.name)),
				None,
				None,
				Some(0.9),
			));
		}
	}

	findings
}

pub fn detect_dependency_misuse(ctx: &CheckContext) -> Vec<DepMisuseFinding> {
	let identity = foch_core::domain::dep_resolution::ModIdentityIndex::from_mods(&ctx.mods);
	let mut findings = Vec::new();

	for mod_item in ctx.mods.iter().filter(|item| item.entry.enabled) {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			let Some(dep_idx) = identity.lookup(dependency) else {
				continue;
			};
			let Some(dep_mod) = ctx.mods.get(dep_idx) else {
				continue;
			};
			if !dep_mod.entry.enabled || dep_mod.mod_id == mod_item.mod_id {
				continue;
			}

			let semantic_refs_to_dep = count_symbol_references_resolving_to_mod(
				&ctx.semantic_index,
				&mod_item.mod_id,
				&dep_mod.mod_id,
			);
			if semantic_refs_to_dep > 0 {
				continue;
			}

			findings.push(DepMisuseFinding {
				mod_id: mod_item.mod_id.clone(),
				mod_display_name: display_name_for_mod(mod_item),
				suspicious_dep_id: dep_mod.mod_id.clone(),
				suspicious_dep_display_name: display_name_for_mod(dep_mod),
				evidence: DepMisuseEvidence {
					semantic_refs_to_dep,
					false_remove_count: 0,
				},
			});
		}
	}

	findings
}

pub fn check_dependency_misuse(ctx: &CheckContext) -> Vec<Finding> {
	detect_dependency_misuse(ctx)
		.into_iter()
		.map(|finding| {
			new_finding(
				"D001",
				Severity::Warning,
				FindingChannel::Advisory,
				format!(
					"descriptor.mod dependencies misuse suspected: {} declares {} without semantic references",
					finding.mod_id, finding.suspicious_dep_id
				),
				Some(finding.mod_id.clone()),
				ctx.mods
					.iter()
					.find(|mod_item| mod_item.mod_id == finding.mod_id)
					.and_then(|mod_item| mod_item.descriptor_path.clone()),
				Some(format!(
					"dep_id={} dep_display_name={} semantic_refs_to_dep={} false_remove_count={}",
					finding.suspicious_dep_id,
					finding.suspicious_dep_display_name,
					finding.evidence.semantic_refs_to_dep,
					finding.evidence.false_remove_count
				)),
				None,
				None,
				Some(0.8),
			)
		})
		.collect()
}

fn display_name_for_mod(mod_item: &foch_core::model::ModCandidate) -> String {
	mod_item
		.entry
		.display_name
		.as_deref()
		.map(str::trim)
		.filter(|name| !name.is_empty())
		.or_else(|| {
			mod_item
				.descriptor
				.as_ref()
				.map(|descriptor| descriptor.name.trim())
				.filter(|name| !name.is_empty())
		})
		.unwrap_or(mod_item.mod_id.as_str())
		.to_string()
}

pub fn check_duplicate_scripted_effect(ctx: &CheckContext) -> Vec<Finding> {
	let mut grouped: HashMap<&str, Vec<_>> = HashMap::new();
	for definition in &ctx.semantic_index.definitions {
		if definition.kind == SymbolKind::ScriptedEffect {
			grouped
				.entry(&definition.name)
				.or_default()
				.push(definition);
		}
	}

	let mut findings = Vec::new();
	for (name, defs) in grouped {
		let mut unique_mods = HashSet::new();
		for def in &defs {
			unique_mods.insert(def.mod_id.as_str());
		}
		if unique_mods.len() < 2 {
			continue;
		}

		let evidence = defs
			.iter()
			.map(|def| format!("{}:{}#L{}", def.mod_id, def.path.display(), def.line))
			.collect::<Vec<_>>()
			.join("; ");
		let Some(last) = defs.last() else {
			continue;
		};
		findings.push(new_finding(
			"R007",
			Severity::Warning,
			FindingChannel::Advisory,
			format!("scripted effect 重复定义: {name}"),
			Some(last.mod_id.clone()),
			Some(last.path.clone()),
			Some(evidence),
			Some(last.line),
			Some(last.column),
			Some(0.8),
		));
	}

	findings
}

#[allow(clippy::too_many_arguments)]
fn new_finding(
	rule_id: &str,
	severity: Severity,
	channel: FindingChannel,
	message: String,
	mod_id: Option<String>,
	path: Option<std::path::PathBuf>,
	evidence: Option<String>,
	line: Option<usize>,
	column: Option<usize>,
	confidence: Option<f32>,
) -> Finding {
	Finding {
		rule_id: rule_id.to_string(),
		severity,
		channel,
		message,
		mod_id,
		path,
		evidence,
		line,
		column,
		confidence,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::{Playlist, PlaylistEntry};
	use foch_core::model::{
		ModCandidate, ScopeType, SemanticIndex, SymbolDefinition, SymbolReference,
	};
	use std::path::PathBuf;

	fn candidate(
		mod_id: &str,
		display_name: &str,
		descriptor_name: &str,
		deps: &[&str],
	) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				display_name: Some(display_name.to_string()),
				enabled: true,
				position: Some(0),
				steam_id: Some(mod_id.to_string()),
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: Some(PathBuf::from(format!("{mod_id}/descriptor.mod"))),
			descriptor: Some(ModDescriptor {
				name: descriptor_name.to_string(),
				dependencies: deps.iter().map(|dep| (*dep).to_string()).collect(),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn definition(mod_id: &str, local_name: &str) -> SymbolDefinition {
		SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: format!("eu4::common.scripted_effects::{local_name}"),
			module: "common.scripted_effects".to_string(),
			local_name: local_name.to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Unknown,
			inferred_this_type: ScopeType::Unknown,
			inferred_this_mask: 0,
			inferred_from_mask: 0,
			inferred_root_mask: 0,
			required_params: Vec::new(),
			optional_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		}
	}

	fn reference(mod_id: &str, name: &str) -> SymbolReference {
		SymbolReference {
			kind: SymbolKind::ScriptedEffect,
			name: name.to_string(),
			module: "common.scripted_effects".to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from("common/scripted_effects/caller.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			provided_params: Vec::new(),
			param_bindings: Vec::new(),
		}
	}

	fn context(mods: Vec<ModCandidate>, semantic_index: SemanticIndex) -> CheckContext {
		CheckContext {
			playlist_path: PathBuf::from("playlist.json"),
			playlist: Playlist {
				mods: mods.iter().map(|mod_item| mod_item.entry.clone()).collect(),
				..Playlist::default()
			},
			mods,
			semantic_index,
		}
	}

	#[test]
	fn flags_declared_dependency_with_no_semantic_refs() {
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Dependency Mod"]),
				candidate("200", "Dependency Mod", "Dependency Mod", &[]),
			],
			SemanticIndex::default(),
		);

		let findings = detect_dependency_misuse(&ctx);

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].mod_id, "100");
		assert_eq!(findings[0].suspicious_dep_id, "200");
		assert_eq!(findings[0].evidence.semantic_refs_to_dep, 0);
		assert_eq!(check_dependency_misuse(&ctx)[0].rule_id, "D001");
	}

	#[test]
	fn does_not_flag_dependency_with_semantic_ref() {
		let mut index = SemanticIndex::default();
		index.definitions.push(definition("200", "shared_effect"));
		index.references.push(reference("100", "shared_effect"));
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Dependency Mod"]),
				candidate("200", "Dependency Mod", "Dependency Mod", &[]),
			],
			index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert!(findings.is_empty());
	}

	#[test]
	fn flags_only_unused_declared_dependency() {
		let mut index = SemanticIndex::default();
		index.definitions.push(definition("200", "used_effect"));
		index.definitions.push(definition("300", "unused_effect"));
		index.references.push(reference("100", "used_effect"));
		let ctx = context(
			vec![
				candidate("100", "Main Mod", "main", &["Used Mod", "Unused Mod"]),
				candidate("200", "Used Mod", "Used Mod", &[]),
				candidate("300", "Unused Mod", "Unused Mod", &[]),
			],
			index,
		);

		let findings = detect_dependency_misuse(&ctx);

		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].suspicious_dep_id, "300");
	}
}
