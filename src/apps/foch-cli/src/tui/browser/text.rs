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
