import type {
	EffectiveLanguage,
	MergeAnalysisStage,
	MergeAnalysisState,
	MergeDisposition,
} from "./api";

export interface Copy {
	tabs: {
		play: string;
		plugins: string;
		merge: string;
		settings: string;
	};
	play: {
		title: string;
		kicker: string;
		loading: string;
		game: string;
		supported: string;
		detected: string;
		notDetected: string;
		versionReady: string;
		versionDifferent: string;
		host: string;
		hostReady: string;
		hostMissing: string;
		companionTitle: string;
		installDeclared: string;
		order: string;
		noOrder: string;
		conflicts: string;
		launch: string;
		prepare: string;
		confirmTitle: string;
		confirmBody: string;
		confirmLaunch: string;
		cancel: string;
		status: string;
		noStatus: string;
		active: string;
		refused: string;
		failed: string;
		notLoaded: string;
		prepared: string;
		started: string;
		planBlocked: string;
	};
	plugins: {
		title: string;
		kicker: string;
		loading: string;
		empty: string;
		install: string;
		installSpec: string;
		sha256: string;
		import: string;
		importPath: string;
		toggle: string;
		notInstalled: string;
		declaredBy: string;
		requiredBy: string;
		config: string;
		enabled: string;
		installed: string;
		missing: string;
		conflict: string;
	};
	settings: {
		title: string;
		kicker: string;
		loading: string;
		transport: string;
		mirror: string;
		language: string;
		transportAuto: string;
		transportHttp: string;
		transportGh: string;
		mirrorAuto: string;
		mirrorChina: string;
		mirrorDirect: string;
		mirrorCustom: string;
		customMirrors: string;
		languageAuto: string;
		saved: string;
		customHint: string;
		testConnectivity: string;
		testingConnectivity: string;
		reachable: string;
		unreachable: string;
		connectivityResults: string;
		sha256Reminder: string;
		latency: string;
	};
	merge: {
		checkingKicker: string;
		checkingTitle: string;
		checkingBody: string;
		startingCheck: string;
		stageNoCount: string;
		installPath: string;
		gamePathLabel: string;
		verifyPath: string;
		useInstall: string;
		verifiedVersion: string;
		buildBase: string;
		cancelBuild: string;
		sessionIgnore: string;
		parsingDocuments: string;
		stages: {
			game_install: string;
			launcher_input: string;
			workshop_acf: string;
			descriptors: string;
			base_data: string;
			detect_version: string;
			collect_inventory: string;
			discover_documents: string;
			parse_documents: string;
			build_semantic_index: string;
			materialize_snapshot: string;
			encode_snapshot: string;
			install: string;
		};
		inspectFailedKicker: string;
		inspectFailedTitle: string;
		checkAgain: string;
		checking: string;
		ready: string;
		availableReady: string;
		incomplete: string;
		blocked: string;
		inputReadiness: string;
		game: string;
		versionMissing: string;
		base: string;
		currentPlayset: string;
		notDetected: string;
		enabledMods: string;
		includedMods: string;
		noPlayset: string;
		localMod: string;
		dependsOn: string;
		unavailablePrefix: string;
		unavailable: string;
		off: string;
		incompleteKicker: string;
		workshopUnavailable: string;
		workshopUnavailablePlural: string;
		launcherUntouched: string;
		omittedLabel: string;
		moreUnavailable: string;
		incompleteWarning: string;
		analyzeAvailable: string;
		analyzeAvailableHint: string;
		analyzeCurrent: string;
		analyzeCurrentHint: string;
		starting: string;
		readOnly: string;
		actionRequired: string;
		resolveBlockers: string;
		noInference: string;
		currentLoadOrder: string;
		readOnlyBody: string;
		cancel: string;
		cancelling: string;
		elapsed: string;
		units: string;
		progress: string;
		updateFailed: string;
		retryUpdate: string;
		reviewKicker: string;
		buildingAvailable: string;
		buildingComplete: string;
		buildingAvailableDetail: string;
		buildingCompleteDetail: string;
		noReview: string;
		cancelledDetail: string;
		endedDetail: string;
		checkInputAgain: string;
		analysisUnavailable: string;
		analysisExpired: string;
		mergeUnits: string;
		results: string;
		search: string;
		searchPlaceholder: string;
		filter: string;
		allOutcomes: string;
		loadingUnits: string;
		unitsUnavailable: string;
		noUnits: string;
		previous: string;
		next: string;
		page: string;
		unitDetail: string;
		loadingDetail: string;
		detailUnavailable: string;
		selectUnit: string;
		selectUnitBody: string;
		evidenceTitle: string;
		evidenceBody: string;
		selectedUnit: string;
		family: string;
		strategy: string;
		unit: string;
		definitionModule: string;
		file: string;
		output: string;
		outcome: string;
		contributors: string;
		baseGame: string;
		notes: string;
		sources: string;
		incompleteReady: string;
		incompleteRequired: string;
		incompleteBlocked: string;
		detectedInput: string;
		readOnlyBadge: string;
		exportTitle: string;
		exportHelp: string;
		exportAction: string;
		confirmExport: string;
		exporting: string;
		exported: string;
		outputFolder: string;
		generatedFiles: string;
		readyExport: string;
		partialExport: string;
		committed: string;
		leafConflictsTitle: string;
		conflictReason: string;
		vanillaSnippet: string;
		competingMods: string;
		preferForThisFile: string;
		preferForFolder: string;
		pendingResolutionsTitle: string;
		pendingResolutionsBody: string;
		removeRule: string;
		clearRules: string;
		reanalyzeWithResolutions: string;
		reanalyzing: string;
		copyFochToml: string;
		hideFochToml: string;
		tomlPreviewNote: string;
		ruleScopeFile: string;
		ruleScopeFolder: string;
		rulePrefers: string;
		disposition: Record<MergeDisposition, string>;
		stage: Record<MergeAnalysisStage, string>;
		state: Record<MergeAnalysisState, string>;
	};
}

const en: Copy = {
	tabs: {
		play: "Play & Launch",
		plugins: "Plugins",
		merge: "Merge & Conflicts",
		settings: "Settings",
	},
	play: {
		title: "Play & Launch",
		kicker: "EU4 1.37.5",
		loading: "Reading plugins, the host DLL, and the current playset.",
		game: "Game",
		supported: "Supported",
		detected: "Detected",
		notDetected: "Not detected",
		versionReady: "Ready",
		versionDifferent: "Different version",
		host: "Host DLL",
		hostReady: "Ready",
		hostMissing: "Missing",
		companionTitle: "Playset mods declare plugins that are not ready",
		installDeclared: "Install",
		order: "Enabled load order",
		noOrder: "No plugins are enabled for this playset.",
		conflicts: "Conflict",
		launch: "Launch EU4",
		prepare: "Prepare Only (Dry-Run)",
		confirmTitle: "Launch Europa Universalis IV?",
		confirmBody:
			"This starts eu4.exe with the staged VERSION.dll and the frozen plugin plan. Prepare Only never starts the game.",
		confirmLaunch: "Start EU4",
		cancel: "Cancel",
		status: "Latest run",
		noStatus: "No launch has been prepared in this session.",
		active: "Active",
		refused: "Refused",
		failed: "Failed",
		notLoaded: "Not loaded",
		prepared: "Prepared only",
		started: "EU4 started",
		planBlocked: "Launch is blocked until the plugin plan is launchable.",
	},
	plugins: {
		title: "Plugins",
		kicker: "Installed, built-in, and declared",
		loading: "Loading the plugin list.",
		empty: "No plugins are registered for this playset.",
		install: "Install",
		installSpec: "owner/repo or owner/repo@tag",
		sha256: "Optional sha256",
		import: "Import directory",
		importPath: "Package directory",
		toggle: "Toggle",
		notInstalled: "Not installed",
		declaredBy: "Declared by",
		requiredBy: "Required by",
		config: "Configuration",
		enabled: "Enabled",
		installed: "Installed",
		missing: "Not installed",
		conflict: "Conflict",
	},
	settings: {
		title: "Settings",
		kicker: "Shared with the CLI and TUI",
		loading: "Loading saved preferences.",
		transport: "Transport",
		mirror: "Mirror",
		language: "Language",
		transportAuto: "Auto (HTTPS → gh)",
		transportHttp: "HTTPS only",
		transportGh: "gh CLI only",
		mirrorAuto: "Auto (China → Direct)",
		mirrorChina: "China Mirrors",
		mirrorDirect: "Direct GitHub",
		mirrorCustom: "Custom",
		customMirrors: "Custom HTTPS prefixes",
		languageAuto: "Auto",
		saved: "Saved",
		customHint: "Separate HTTPS prefixes with commas.",
		testConnectivity: "Test Connectivity",
		testingConnectivity: "Testing connectivity…",
		reachable: "Reachable",
		unreachable: "Unreachable",
		connectivityResults: "Connectivity",
		sha256Reminder:
			"Mirror downloads still enforce SHA-256 and PE export validation before anything is installed.",
		latency: "Latency",
	},
	merge: {
		checkingKicker: "First-launch inspection",
		checkingTitle: "Checking EU4 setup",
		checkingBody: "Detecting the game, base data, and current launcher playset.",
		startingCheck: "Starting the read-only check.",
		stageNoCount: "This stage has no item count yet.",
		installPath: "Install path",
		gamePathLabel: "EU4 installation directory",
		verifyPath: "Verify path",
		useInstall: "Use this EU4 install",
		verifiedVersion: "Verified version",
		buildBase: "Build base data from this install",
		cancelBuild: "Cancel base data build",
		sessionIgnore: "Ignore missing items for this session only.",
		parsingDocuments: "Parsing game documents",
		stages: {
			game_install: "Reading the EU4 installation",
			launcher_input: "Reading the Launcher playset",
			workshop_acf: "Reading the Workshop installation record",
			descriptors: "Reading mod descriptors",
			base_data: "Checking installed base data",
			detect_version: "Detecting the EU4 version",
			collect_inventory: "Listing game files",
			discover_documents: "Finding documents to parse",
			parse_documents: "Parsing game documents",
			build_semantic_index: "Building the semantic index",
			materialize_snapshot: "Building the base snapshot",
			encode_snapshot: "Encoding the base snapshot",
			install: "Installing base data",
		},
		inspectFailedKicker: "Input inspection failed",
		inspectFailedTitle: "EU4 setup could not be inspected",
		checkAgain: "Check again",
		checking: "Checking…",
		ready: "Ready to analyze",
		availableReady: "Available mods ready",
		incomplete: "Incomplete input",
		blocked: "Setup blocked",
		inputReadiness: "Input readiness",
		game: "Game",
		versionMissing: "Not detected",
		base: "EU4 base",
		currentPlayset: "Current playset",
		notDetected: "Not detected",
		enabledMods: "Enabled mods",
		includedMods: "Included mods / source playset mods",
		noPlayset: "No current EU4 playset was found.",
		localMod: "Local mod",
		dependsOn: "Depends on:",
		unavailablePrefix: "Unavailable:",
		unavailable: "Unavailable",
		off: "Off",
		incompleteKicker: "Incomplete playset",
		workshopUnavailable: "Workshop mod unavailable",
		workshopUnavailablePlural: "Workshop mods unavailable",
		launcherUntouched: "The Launcher playset and every mod file stay untouched.",
		omittedLabel: "Unavailable Workshop mods",
		moreUnavailable: "more unavailable",
		incompleteWarning:
			"The review will be marked incomplete because unavailable mods cannot contribute to its outcomes.",
		analyzeAvailable: "Analyze available mods",
		analyzeAvailableHint: "Results will be marked incomplete",
		analyzeCurrent: "Analyze current playset",
		analyzeCurrentHint: "No mod files will be changed",
		starting: "Starting analysis…",
		readOnly: "Read-only analysis",
		actionRequired: "Action required",
		resolveBlockers: "Resolve setup blockers before analysis",
		noInference: "Foch will not infer missing game or base-data inputs.",
		currentLoadOrder: "Current load order",
		readOnlyBody:
			"Foch reads the detected load order and prepares a complete review. It does not change Launcher settings, game files, or mod files.",
		cancel: "Cancel analysis",
		cancelling: "Cancelling…",
		elapsed: "elapsed",
		units: "units",
		progress: "Merge analysis progress",
		updateFailed: "Analysis update failed:",
		retryUpdate: "Retry update",
		reviewKicker: "Merge unit review",
		buildingAvailable: "Building the available-mod review",
		buildingComplete: "Building the complete review",
		buildingAvailableDetail:
			"Results appear after every included merge unit has reached a final outcome. Unavailable mods are not represented.",
		buildingCompleteDetail:
			"Results appear after every merge unit has reached a final outcome. No partial review is shown.",
		noReview: "No review was produced",
		cancelledDetail:
			"The analysis stopped before it produced final outcomes. Check the current input before starting again.",
		endedDetail:
			"The analysis ended before a review was available. Check the current input, then run it again.",
		checkInputAgain: "Check input again",
		analysisUnavailable: "Analysis unavailable",
		analysisExpired:
			"The analysis record is no longer available. Check the current input before starting again.",
		mergeUnits: "Merge units",
		results: "results",
		search: "Search",
		searchPlaceholder: "Search path or family",
		filter: "Filter merge units by outcome",
		allOutcomes: "All outcomes",
		loadingUnits: "Loading merge units…",
		unitsUnavailable: "Merge units unavailable:",
		noUnits: "No merge units match this view.",
		previous: "Previous",
		next: "Next",
		page: "Page",
		unitDetail: "Merge unit detail",
		loadingDetail: "Loading unit detail…",
		detailUnavailable: "Unit detail unavailable:",
		selectUnit: "Select a merge unit",
		selectUnitBody: "Choose a row to inspect its outcome and ordered contributors.",
		evidenceTitle: "Analysis evidence appears here",
		evidenceBody:
			"Foch exposes focused outcomes and contributors, never raw syntax trees.",
		selectedUnit: "Selected merge unit",
		family: "Family",
		strategy: "Strategy",
		unit: "Unit",
		definitionModule: "Definition module",
		file: "File",
		output: "Output",
		outcome: "Outcome",
		contributors: "Load-order contributors",
		baseGame: "Base game",
		notes: "Review notes",
		sources: "sources",
		incompleteReady: "Incomplete review ready",
		incompleteRequired: "Incomplete review required",
		incompleteBlocked: "Incomplete review blocked",
		detectedInput: "Detected input",
		readOnlyBadge: "Read-only analysis",
		exportTitle: "Export Merged Mod",
		exportHelp:
			"Choose a new or empty folder. Export never edits Steam, the game directory, or source mods. A folder that already contains files is refused.",
		exportAction: "Export Merged Mod",
		confirmExport: "Confirm export to this folder",
		exporting: "Exporting…",
		exported: "Exported",
		outputFolder: "Output folder",
		generatedFiles: "Generated files",
		readyExport:
			"Safe files were written to an empty folder. Source mods and the game directory were not modified.",
		partialExport:
			"Safe files and definition modules were written. Unsafe units were deferred and were not overwritten.",
		committed: "This analysis has already been exported.",
		leafConflictsTitle: "Leaf conflicts",
		conflictReason: "Why it deferred",
		vanillaSnippet: "Vanilla",
		competingMods: "Competing mods",
		preferForThisFile: "Prefer for this file",
		preferForFolder: "Prefer for folder",
		pendingResolutionsTitle: "Pending resolutions",
		pendingResolutionsBody:
			"These explicit foch.toml rules will be applied to a fresh session analysis. Nothing is written to your mods or playset.",
		removeRule: "Remove",
		clearRules: "Clear all",
		reanalyzeWithResolutions: "Re-analyze with resolutions",
		reanalyzing: "Re-analyzing…",
		copyFochToml: "Copy foch.toml snippet",
		hideFochToml: "Hide foch.toml snippet",
		tomlPreviewNote:
			"Add this [[resolutions]] block to your own foch.toml to keep the same choices in the CLI.",
		ruleScopeFile: "file",
		ruleScopeFolder: "folder",
		rulePrefers: "prefers",
		disposition: {
			safe: "Safe merge",
			copy: "Copy through",
			needs_user_choice: "Needs a choice",
			unsupported_input: "Unsupported input",
			engine_failure: "Engine failure",
			deferred: "Deferred",
		},
		stage: {
			inventory: "Scan load order",
			resolve_input: "Read playset",
			semantic_merge: "Compare merge units",
			validate_output: "Check results",
			freeze_artifacts: "Prepare review",
			complete: "Complete",
		},
		state: {
			queued: "Queued",
			running: "Analyzing",
			ready: "Review ready",
			ready_with_deferrals: "Review required",
			blocked: "Review blocked",
			cancelled: "Cancelled",
			failed: "Analysis failed",
		},
	},
};

const zh: Copy = {
	tabs: {
		play: "启动与运行",
		plugins: "插件",
		merge: "合并与冲突",
		settings: "设置",
	},
	play: {
		title: "启动与运行",
		kicker: "EU4 1.37.5",
		loading: "正在读取插件、宿主 DLL 和当前剧本组。",
		game: "游戏",
		supported: "支持版本",
		detected: "检测到",
		notDetected: "未检测到",
		versionReady: "就绪",
		versionDifferent: "版本不同",
		host: "宿主 DLL",
		hostReady: "就绪",
		hostMissing: "缺失",
		companionTitle: "剧本组模组声明了尚未就绪的插件",
		installDeclared: "安装",
		order: "已启用的加载顺序",
		noOrder: "这个剧本组没有启用插件。",
		conflicts: "冲突",
		launch: "启动 EU4",
		prepare: "仅准备（空跑）",
		confirmTitle: "启动欧陆风云 IV？",
		confirmBody:
			"这将用已暂存的 VERSION.dll 和冻结的插件计划启动 eu4.exe。仅准备不会启动游戏。",
		confirmLaunch: "启动 EU4",
		cancel: "取消",
		status: "最近一次运行",
		noStatus: "这次会话还没有准备启动。",
		active: "已激活",
		refused: "已拒绝",
		failed: "失败",
		notLoaded: "未加载",
		prepared: "仅已准备",
		started: "EU4 已启动",
		planBlocked: "插件计划不可启动时，启动会被阻止。",
	},
	plugins: {
		title: "插件",
		kicker: "已安装、内置与模组声明",
		loading: "正在加载插件列表。",
		empty: "这个剧本组没有登记插件。",
		install: "安装",
		installSpec: "owner/repo 或 owner/repo@tag",
		sha256: "可选 sha256",
		import: "导入目录",
		importPath: "插件包目录",
		toggle: "切换",
		notInstalled: "未安装",
		declaredBy: "声明来自",
		requiredBy: "要求来自",
		config: "配置",
		enabled: "已启用",
		installed: "已安装",
		missing: "未安装",
		conflict: "冲突",
	},
	settings: {
		title: "设置",
		kicker: "与命令行和 TUI 共用",
		loading: "正在加载已保存的偏好。",
		transport: "传输",
		mirror: "镜像",
		language: "语言",
		transportAuto: "自动（HTTPS → gh）",
		transportHttp: "仅 HTTPS",
		transportGh: "仅 gh CLI",
		mirrorAuto: "自动（中国镜像 → 直连）",
		mirrorChina: "中国镜像",
		mirrorDirect: "直连 GitHub",
		mirrorCustom: "自定义",
		customMirrors: "自定义 HTTPS 前缀",
		languageAuto: "自动",
		saved: "已保存",
		customHint: "多个 HTTPS 前缀用逗号分隔。",
		testConnectivity: "测试连通性",
		testingConnectivity: "正在测试连通性…",
		reachable: "可达",
		unreachable: "不可达",
		connectivityResults: "连通性",
		sha256Reminder: "镜像下载在安装前仍会强制校验 SHA-256，并检查 PE 导出表。",
		latency: "延迟",
	},
	merge: {
		checkingKicker: "首次检查",
		checkingTitle: "正在检查 EU4 环境",
		checkingBody: "正在检测游戏、基础数据和当前启动器剧本组。",
		startingCheck: "正在开始只读检查。",
		stageNoCount: "这个阶段还没有条目计数。",
		installPath: "安装路径",
		gamePathLabel: "EU4 安装目录",
		verifyPath: "验证路径",
		useInstall: "使用这个 EU4 安装",
		verifiedVersion: "已验证版本",
		buildBase: "从这次安装构建基础数据",
		cancelBuild: "取消基础数据构建",
		sessionIgnore: "仅本次忽略缺失项。",
		parsingDocuments: "正在解析游戏文档",
		stages: {
			game_install: "正在读取 EU4 安装",
			launcher_input: "正在读取启动器剧本组",
			workshop_acf: "正在读取创意工坊安装记录",
			descriptors: "正在读取模组描述文件",
			base_data: "正在检查已安装的基础数据",
			detect_version: "正在检测 EU4 版本",
			collect_inventory: "正在列出游戏文件",
			discover_documents: "正在查找要解析的文档",
			parse_documents: "正在解析游戏文档",
			build_semantic_index: "正在建立语义索引",
			materialize_snapshot: "正在建立基础快照",
			encode_snapshot: "正在编码基础快照",
			install: "正在安装基础数据",
		},
		inspectFailedKicker: "输入检查失败",
		inspectFailedTitle: "无法检查 EU4 环境",
		checkAgain: "重新检查",
		checking: "正在检查…",
		ready: "可以分析",
		availableReady: "可用模组已就绪",
		incomplete: "输入不完整",
		blocked: "环境受阻",
		inputReadiness: "输入就绪",
		game: "游戏",
		versionMissing: "未检测到",
		base: "EU4 基础数据",
		currentPlayset: "当前剧本组",
		notDetected: "未检测到",
		enabledMods: "已启用模组",
		includedMods: "纳入模组 / 剧本组模组",
		noPlayset: "没有找到当前的 EU4 剧本组。",
		localMod: "本地模组",
		dependsOn: "依赖于：",
		unavailablePrefix: "不可用：",
		unavailable: "不可用",
		off: "关闭",
		incompleteKicker: "剧本组不完整",
		workshopUnavailable: "个创意工坊模组不可用",
		workshopUnavailablePlural: "个创意工坊模组不可用",
		launcherUntouched: "启动器剧本组和所有模组文件都不会被改动。",
		omittedLabel: "不可用的创意工坊模组",
		moreUnavailable: "个更多不可用项",
		incompleteWarning: "不可用模组无法参与结果，因此审查会被标成不完整。",
		analyzeAvailable: "分析可用模组",
		analyzeAvailableHint: "结果会被标成不完整",
		analyzeCurrent: "分析当前剧本组",
		analyzeCurrentHint: "不会改动任何模组文件",
		starting: "正在开始分析…",
		readOnly: "只读分析",
		actionRequired: "需要处理",
		resolveBlockers: "分析前请先解决环境问题",
		noInference: "Foch 不会猜测缺失的游戏或基础数据。",
		currentLoadOrder: "当前加载顺序",
		readOnlyBody:
			"Foch 读取检测到的加载顺序并准备完整审查。它不会改动启动器设置、游戏文件或模组文件。",
		cancel: "取消分析",
		cancelling: "正在取消…",
		elapsed: "已用时",
		units: "个单元",
		progress: "合并分析进度",
		updateFailed: "分析更新失败：",
		retryUpdate: "重试更新",
		reviewKicker: "合并单元审查",
		buildingAvailable: "正在建立可用模组审查",
		buildingComplete: "正在建立完整审查",
		buildingAvailableDetail:
			"每个纳入的合并单元都有最终结果后才会显示。不可用模组不会出现。",
		buildingCompleteDetail: "每个合并单元都有最终结果后才会显示。不会展示部分审查。",
		noReview: "没有产生审查",
		cancelledDetail: "分析在产生最终结果前停止了。请先检查当前输入再重新开始。",
		endedDetail: "分析在审查可用前结束了。请检查当前输入后再运行一次。",
		checkInputAgain: "重新检查输入",
		analysisUnavailable: "分析不可用",
		analysisExpired: "分析记录已经不在了。请先检查当前输入再重新开始。",
		mergeUnits: "合并单元",
		results: "条结果",
		search: "搜索",
		searchPlaceholder: "搜索路径或类别",
		filter: "按结果筛选合并单元",
		allOutcomes: "全部结果",
		loadingUnits: "正在加载合并单元…",
		unitsUnavailable: "合并单元不可用：",
		noUnits: "没有符合这个视图的合并单元。",
		previous: "上一页",
		next: "下一页",
		page: "第",
		unitDetail: "合并单元详情",
		loadingDetail: "正在加载单元详情…",
		detailUnavailable: "单元详情不可用：",
		selectUnit: "选择一个合并单元",
		selectUnitBody: "选择一行以查看结果和按加载顺序排列的贡献者。",
		evidenceTitle: "分析证据显示在这里",
		evidenceBody: "Foch 展示聚焦的结果和贡献者，不展示原始语法树。",
		selectedUnit: "已选合并单元",
		family: "类别",
		strategy: "策略",
		unit: "单元",
		definitionModule: "定义模块",
		file: "文件",
		output: "输出",
		outcome: "结果",
		contributors: "加载顺序贡献者",
		baseGame: "原版游戏",
		notes: "审查备注",
		sources: "个来源",
		incompleteReady: "不完整审查已就绪",
		incompleteRequired: "不完整审查需要处理",
		incompleteBlocked: "不完整审查受阻",
		detectedInput: "检测到的输入",
		readOnlyBadge: "只读分析",
		exportTitle: "导出合并 Mod",
		exportHelp:
			"请选择一个新建或空的文件夹。导出不会修改 Steam、游戏目录或源 Mod。已有文件的文件夹会被拒绝。",
		exportAction: "导出合并 Mod",
		confirmExport: "确认导出到此文件夹",
		exporting: "正在导出…",
		exported: "已导出",
		outputFolder: "输出文件夹",
		generatedFiles: "生成的文件",
		readyExport: "已把安全文件写入空文件夹。源 Mod 和游戏目录没有被修改。",
		partialExport: "已写入安全的文件和定义模块。不安全的单元已延后，没有被覆盖。",
		committed: "这次分析已经导出。",
		leafConflictsTitle: "叶子冲突",
		conflictReason: "延后原因",
		vanillaSnippet: "原版",
		competingMods: "竞争 Mod",
		preferForThisFile: "本文件优先此 Mod",
		preferForFolder: "同目录优先此 Mod",
		pendingResolutionsTitle: "待应用解决规则",
		pendingResolutionsBody:
			"这些明确的 foch.toml 规则将应用到一次新的会话分析。不会写入你的 Mod 或剧本组。",
		removeRule: "移除",
		clearRules: "全部清除",
		reanalyzeWithResolutions: "应用规则并重新分析",
		reanalyzing: "正在重新分析…",
		copyFochToml: "复制 foch.toml 配置",
		hideFochToml: "收起 foch.toml 配置",
		tomlPreviewNote:
			"把这段 [[resolutions]] 加入你自己的 foch.toml，即可在命令行复用相同选择。",
		ruleScopeFile: "文件",
		ruleScopeFolder: "目录",
		rulePrefers: "优先",
		disposition: {
			safe: "安全合并",
			copy: "原样复制",
			needs_user_choice: "需要选择",
			unsupported_input: "不支持的输入",
			engine_failure: "引擎失败",
			deferred: "延后",
		},
		stage: {
			inventory: "扫描加载顺序",
			resolve_input: "读取剧本组",
			semantic_merge: "比较合并单元",
			validate_output: "检查结果",
			freeze_artifacts: "准备审查",
			complete: "完成",
		},
		state: {
			queued: "已排队",
			running: "正在分析",
			ready: "审查就绪",
			ready_with_deferrals: "需要审查",
			blocked: "审查受阻",
			cancelled: "已取消",
			failed: "分析失败",
		},
	},
};

export function copyFor(language: EffectiveLanguage): Copy {
	return language === "zh-CN" ? zh : en;
}
