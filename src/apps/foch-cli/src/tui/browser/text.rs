//! Interface copy for the bare `foch` browser, in simplified Chinese and English.

use foch::plugin::UiLanguage;

pub fn tab_launch(language: UiLanguage) -> String {
	match language {
		UiLanguage::ZhCn => "[1] 启动与插件".to_string(),
		UiLanguage::En => "[1] Launch & Plugins".to_string(),
	}
}

pub fn tab_merge(language: UiLanguage, attention: usize) -> String {
	let base = match language {
		UiLanguage::ZhCn => "[2] 合并审阅",
		UiLanguage::En => "[2] Merge Review",
	};
	if attention == 0 {
		base.to_string()
	} else {
		format!("{base} (!{attention})")
	}
}

pub fn tab_settings(language: UiLanguage) -> String {
	match language {
		UiLanguage::ZhCn => "[3] 网络与设置".to_string(),
		UiLanguage::En => "[3] Network & Settings".to_string(),
	}
}

pub fn badge(language: UiLanguage, badge: foch::plugin::PluginBadge) -> &'static str {
	use foch::plugin::PluginBadge::*;
	match (language, badge) {
		(UiLanguage::ZhCn, Enabled) => "[已启用]",
		(UiLanguage::ZhCn, Installed) => "[已安装]",
		(UiLanguage::ZhCn, NotInstalled) => "[未安装]",
		(UiLanguage::ZhCn, Conflict) => "[冲突]",
		(UiLanguage::En, Enabled) => "[ENABLED]",
		(UiLanguage::En, Installed) => "[INSTALLED]",
		(UiLanguage::En, NotInstalled) => "[NOT INSTALLED]",
		(UiLanguage::En, Conflict) => "[CONFLICT]",
	}
}

pub fn companion_alert(language: UiLanguage, mod_name: &str, plugin_id: &str) -> String {
	match language {
		UiLanguage::ZhCn => {
			format!("模组「{mod_name}」建议插件 {plugin_id} — 按 [i] 安装 / [Space] 启用")
		}
		UiLanguage::En => format!(
			"Mod \"{mod_name}\" recommends plugin {plugin_id} — press [i] to install / [Space] to enable"
		),
	}
}

pub fn transport_label(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "下载传输",
		UiLanguage::En => "Download transport",
	}
}

pub fn mirror_label(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "GitHub 镜像",
		UiLanguage::En => "GitHub mirror",
	}
}

pub fn language_label(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "语言",
		UiLanguage::En => "Language",
	}
}

pub fn transport_value(
	language: UiLanguage,
	value: foch::plugin::TransportPreference,
) -> &'static str {
	use foch::plugin::TransportPreference::*;
	match (language, value) {
		(UiLanguage::ZhCn, Auto) => "自动（HTTPS → gh）",
		(UiLanguage::ZhCn, Http) => "仅 HTTPS",
		(UiLanguage::ZhCn, Gh) => "仅 gh",
		(UiLanguage::En, Auto) => "Auto (HTTPS → gh)",
		(UiLanguage::En, Http) => "HTTPS only",
		(UiLanguage::En, Gh) => "gh CLI only",
	}
}

pub fn mirror_value(language: UiLanguage, value: foch::plugin::MirrorPreference) -> &'static str {
	use foch::plugin::MirrorPreference::*;
	match (language, value) {
		(UiLanguage::ZhCn, Auto) => "自动（中国 → 直连）",
		(UiLanguage::ZhCn, Cn) => "中国镜像（gh-proxy / ghfast）",
		(UiLanguage::ZhCn, Direct) => "直连 GitHub",
		(UiLanguage::ZhCn, Custom) => "自定义",
		(UiLanguage::En, Auto) => "Auto (China → Direct)",
		(UiLanguage::En, Cn) => "China mirrors (gh-proxy / ghfast)",
		(UiLanguage::En, Direct) => "Direct GitHub",
		(UiLanguage::En, Custom) => "Custom",
	}
}

pub fn language_value(
	language: UiLanguage,
	value: foch::plugin::LanguagePreference,
) -> &'static str {
	use foch::plugin::LanguagePreference::*;
	match (language, value) {
		(_, Auto) => "Auto",
		(_, ZhCn) => "简体中文",
		(_, En) => "English",
	}
}

pub fn connectivity_action(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "测试连通性",
		UiLanguage::En => "test connectivity",
	}
}

pub fn route_status(language: UiLanguage, reachable: bool) -> &'static str {
	match (language, reachable) {
		(UiLanguage::ZhCn, true) => "可达",
		(UiLanguage::ZhCn, false) => "不可达",
		(UiLanguage::En, true) => "reachable",
		(UiLanguage::En, false) => "unreachable",
	}
}

pub fn route_line(
	language: UiLanguage,
	label: &str,
	reachable: bool,
	http_status: Option<u16>,
	latency_ms: u64,
	detail: &str,
) -> String {
	let status = route_status(language, reachable);
	let http = match http_status {
		Some(status) => format!(" HTTP {status}"),
		None => String::new(),
	};
	format!("{label} {status}{http} {latency_ms} ms {detail}")
}

pub fn sha256_reminder(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "镜像下载在安装前仍会强制校验 SHA-256，并检查 PE 导出表。",
		UiLanguage::En => {
			"Mirror downloads still enforce SHA-256 and PE export validation before anything is installed."
		}
	}
}

pub fn not_installed(language: UiLanguage, id: &str) -> String {
	match language {
		UiLanguage::ZhCn => format!("插件 {id} 尚未安装，按 i 安装"),
		UiLanguage::En => format!("Plugin {id} is not installed; press i to install"),
	}
}

pub fn leaf_conflicts_heading(language: UiLanguage, count: usize) -> String {
	match language {
		UiLanguage::ZhCn => format!("叶子冲突 ({count})"),
		UiLanguage::En => format!("Leaf conflicts ({count})"),
	}
}

pub fn conflict_kind_line(
	language: UiLanguage,
	kind: Option<&str>,
	reason: Option<&str>,
) -> String {
	let kind = kind.unwrap_or(match language {
		UiLanguage::ZhCn => "冲突",
		UiLanguage::En => "conflict",
	});
	match (language, reason.filter(|reason| !reason.is_empty())) {
		(UiLanguage::ZhCn, Some(reason)) => format!("  类型 {kind} · {reason}"),
		(UiLanguage::ZhCn, None) => format!("  类型 {kind}"),
		(UiLanguage::En, Some(reason)) => format!("  {kind} · {reason}"),
		(UiLanguage::En, None) => format!("  {kind}"),
	}
}

pub fn vanilla_heading() -> &'static str {
	"Vanilla / 原版"
}

pub fn candidate_meta(language: UiLanguage, mod_id: &str, precedence: usize) -> String {
	match language {
		UiLanguage::ZhCn => format!("  {mod_id} · 优先级 {precedence}"),
		UiLanguage::En => format!("  {mod_id} · precedence {precedence}"),
	}
}

pub fn more_conflicts(language: UiLanguage, hidden: usize) -> String {
	match language {
		UiLanguage::ZhCn => format!("  还有 {hidden} 个冲突"),
		UiLanguage::En => format!("  +{hidden} more conflicts"),
	}
}

pub fn more_lines(language: UiLanguage, hidden: usize) -> String {
	match language {
		UiLanguage::ZhCn => format!("    还有 {hidden} 行"),
		UiLanguage::En => format!("    +{hidden} more lines"),
	}
}

pub fn resolution_detail_hint(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => {
			"按 t 复制 foch.toml [[resolutions]] 解决规则模板。注释里列出了其他竞争模组。"
		}
		UiLanguage::En => {
			"Press t to copy a foch.toml [[resolutions]] template. Commented lines list the other mods."
		}
	}
}

pub fn resolution_footer_action(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "解决规则模板",
		UiLanguage::En => "resolution template",
	}
}

pub fn resolution_help_action(language: UiLanguage) -> &'static str {
	match language {
		UiLanguage::ZhCn => "复制 foch.toml 解决规则模板",
		UiLanguage::En => "copy a foch.toml resolution template",
	}
}

pub fn copied_resolution_notice(language: UiLanguage) -> String {
	match language {
		UiLanguage::ZhCn => {
			"已复制 foch.toml 解决规则模板。粘贴到 foch.toml 后才会生效。未改动任何文件。".to_string()
		}
		UiLanguage::En => "Copied foch.toml resolution template. Paste it into foch.toml to apply it. Nothing was changed.".to_string(),
	}
}

pub fn no_resolution_notice(language: UiLanguage) -> String {
	match language {
		UiLanguage::ZhCn => {
			"未复制任何内容。这个单位没有可写入 foch.toml [[resolutions]] 的文件级叶子冲突。"
				.to_string()
		}
		UiLanguage::En => "Nothing was copied. This unit has no file-scoped leaf conflict to turn into a foch.toml [[resolutions]] template.".to_string(),
	}
}

pub fn resolution_template_preamble(language: UiLanguage, path: &str) -> String {
	match language {
		UiLanguage::ZhCn => format!(
			"# foch.toml [[resolutions]] 解决规则模板：{path}\n\
			 # 生效规则采用优先级最高的候选模组。\n\
			 # 其余竞争模组与处理器写在注释里，取消注释即可改用。\n\
			 # 粘贴到 foch.toml 后才会生效。复制不会改动源模组、dlc_load.json 或 foch.toml。"
		),
		UiLanguage::En => format!(
			"# foch.toml [[resolutions]] template for {path}\n\
			 # The active rule prefers the highest-precedence candidate.\n\
			 # Competing mods and handlers below are commented so you can choose one.\n\
			 # Paste into foch.toml to apply it. Copying changes no source mod, dlc_load.json, or foch.toml."
		),
	}
}

pub fn choice_comment(language: UiLanguage, name: &str, precedence: usize) -> String {
	match language {
		UiLanguage::ZhCn => format!("# {name}（优先级 {precedence}）"),
		UiLanguage::En => format!("# {name} (precedence {precedence})"),
	}
}

pub fn handler_comment(language: UiLanguage, handler: &str) -> String {
	match language {
		UiLanguage::ZhCn => format!("# 处理器 {handler}（注释中的备选）"),
		UiLanguage::En => format!("# handler {handler} (commented alternative)"),
	}
}
