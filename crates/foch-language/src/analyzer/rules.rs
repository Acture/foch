use foch_core::model::{CheckContext, Finding, FindingChannel, Severity, SymbolKind};
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
	let mut available_ids = HashSet::new();
	let mut available_names = HashSet::new();
	for mod_item in &ctx.mods {
		available_ids.insert(mod_item.mod_id.clone());
		if let Some(descriptor) = mod_item.descriptor.as_ref() {
			available_names.insert(descriptor.name.clone());
		}
	}

	let mut findings = Vec::new();
	for mod_item in &ctx.mods {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};

		for dependency in &descriptor.dependencies {
			if available_ids.contains(dependency) || available_names.contains(dependency) {
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
