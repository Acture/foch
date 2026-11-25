import { invoke } from "@tauri-apps/api/core";

export const MERGE_DISPOSITIONS = [
	"safe",
	"copy",
	"needs_user_choice",
	"unsupported_input",
	"engine_failure",
	"deferred",
] as const;

export type MergeDisposition = (typeof MERGE_DISPOSITIONS)[number];
export type ReadinessState = "ready" | "ready_with_omissions" | "blocked";
export type AnalysisInputMode = "complete" | "without_unavailable_mods";

export interface ReadinessIssue {
	id: string;
	title: string;
	detail: string;
	action?: string;
}

export interface InstalledGameView {
	name: string;
	version: string | null;
	installPath: string | null;
}

export interface BaseDataView {
	state: "ready" | "missing" | "stale";
	version: string | null;
	detail: string;
}

export interface PlaysetModView {
	id: string;
	name: string;
	position: number;
	enabled: boolean;
	workshopId: string | null;
	workshopManifestId: string | null;
	version: string | null;
	declaredDependencies: string[];
	descriptorPath: string | null;
	sourceError: string | null;
}

export interface DetectedPlaysetView {
	name: string;
	sourcePath: string;
	mods: PlaysetModView[];
}

export interface OmittedPlaysetMod {
	id: string;
	name: string;
	position: number;
	reason: string;
}

export interface InputRecoveryOption {
	kind: AnalysisInputMode;
	sourceModCount: number;
	omittedMods: OmittedPlaysetMod[];
	omittedModCount: number;
	includedModCount: number;
}

export interface InputInspection {
	inspectionId: string;
	readiness: ReadinessState;
	game: InstalledGameView;
	baseData: BaseDataView;
	playset: DetectedPlaysetView | null;
	issues: ReadinessIssue[];
	recovery: InputRecoveryOption | null;
}

export type MergeAnalysisState =
	| "queued"
	| "running"
	| "ready"
	| "ready_with_deferrals"
	| "blocked"
	| "cancelled"
	| "failed";

export type MergeAnalysisStage =
	| "inventory"
	| "resolve_input"
	| "semantic_merge"
	| "validate_output"
	| "freeze_artifacts"
	| "complete";

export interface MergeUnitCounts {
	total: number;
	safe: number;
	copy: number;
	needsUserChoice: number;
	unsupportedInput: number;
	engineFailure: number;
	deferred: number;
}

export interface StartMergeAnalysisResult {
	analysisId: string;
	inputScope: AnalysisInputScope;
}

export interface AnalysisInputScope {
	mode: AnalysisInputMode;
	sourceModCount: number;
	omittedMods: OmittedPlaysetMod[];
	omittedModCount: number;
	includedModCount: number;
}

export interface MergeAnalysisSummary {
	analysisId: string;
	state: MergeAnalysisState;
	stage: MergeAnalysisStage;
	completedUnits: number;
	totalUnits: number;
	elapsedMs: number;
	counts: MergeUnitCounts;
	message: string | null;
}

export interface MergeCommitResult {
	analysisId: string;
	outputDir: string;
	mergeStatus: string;
	generatedFileCount: number;
	manualConflictCount: number;
	unsupportedInputCount: number;
	engineFailureCount: number;
	committedAtNote: string;
}

export interface DesktopRouteProbe {
	label: string;
	url: string;
	kind: "mirror" | "direct" | "gh" | string;
	reachable: boolean;
	httpStatus: number | null;
	latencyMs: number;
	detail: string;
}

export interface DesktopConnectivityReport {
	transport: string;
	mirrorPolicy: string;
	routes: DesktopRouteProbe[];
}

export interface DesktopCommandError {
	code: string | null;
	message: string;
}

export interface MergeUnitListItem {
	id: string;
	path: string;
	family: string;
	kind: "file" | "definition_module";
	disposition: MergeDisposition;
	strategy: string;
	contributorCount: number;
}

export interface MergeUnitListRequest {
	analysisId: string;
	query: string;
	disposition: MergeDisposition | null;
	page: number;
	pageSize: number;
}

export interface MergeUnitPage {
	items: MergeUnitListItem[];
	total: number;
	page: number;
	pageSize: number;
}

export interface MergeUnitContributor {
	modId: string;
	name: string;
	position: number;
	isBaseGame: boolean;
}

export interface MergeUnitDetail {
	id: string;
	path: string;
	family: string;
	kind: "file" | "definition_module";
	disposition: MergeDisposition;
	strategy: string;
	summary: string;
	outputPath: string | null;
	contributors: MergeUnitContributor[];
	notes: string[];
}

export type TransportPreference = "auto" | "http" | "gh";
export type MirrorPreference = "auto" | "cn" | "direct" | "custom";
export type LanguagePreference = "auto" | "zh_cn" | "en";
export type EffectiveLanguage = "zh-CN" | "en";
export type DesktopPluginBadge = "enabled" | "installed" | "not_installed" | "conflict";

export interface DesktopPreferences {
	transport: TransportPreference;
	mirror: MirrorPreference;
	customMirrors: string[];
	language: LanguagePreference;
	effectiveLanguage: EffectiveLanguage;
}

export interface DesktopSettingsInput {
	transport: TransportPreference;
	mirror: MirrorPreference;
	customMirrors: string[];
	language: LanguagePreference;
}

export interface DesktopConfigField {
	key: string;
	kind: string;
	description: string | null;
	value: boolean | number | string | null;
	min: number | null;
	max: number | null;
}

export interface DesktopPluginEntry {
	id: string;
	name: string;
	description: string | null;
	installedVersions: string[];
	selectedVersion: string | null;
	enabled: boolean;
	installSpec: string | null;
	declaredByMods: string[];
	requiredByMods: string[];
	conflicts: string[];
	configFields: DesktopConfigField[];
	badge: DesktopPluginBadge;
}

export interface DesktopPluginPlan {
	launchable: boolean;
	order: string[];
	errors: string[];
	warnings: string[];
}

export interface DesktopPluginOverview {
	playset: string;
	gameRoot: string | null;
	gameVersion: string | null;
	hostDllAvailable: boolean;
	hostDllDetail: string;
	entries: DesktopPluginEntry[];
	companionWarnings: string[];
	plan: DesktopPluginPlan | null;
	preferences: DesktopPreferences;
	problems: string[];
}

export interface DesktopPluginInstall {
	id: string;
	version: string;
	source: string;
	conflicts: string[];
	alreadyPresent: boolean;
}

export interface DesktopLaunchResult {
	runDir: string;
	planSha256: string;
	preparedOnly: boolean;
	pid: number | null;
	pluginCount: number;
	versionDll: boolean;
}

export interface DesktopPluginRuntimeState {
	id: string;
	state: string;
	reason: string | null;
}

export interface DesktopLaunchStatus {
	runId: string;
	plugins: DesktopPluginRuntimeState[];
}

export interface DesktopClient {
	inspectInput(): Promise<InputInspection>;
	startMergeAnalysis(
		inspectionId: string,
		inputMode: AnalysisInputMode,
	): Promise<StartMergeAnalysisResult>;
	cancelMergeAnalysis(analysisId: string): Promise<void>;
	getMergeAnalysisSummary(analysisId: string): Promise<MergeAnalysisSummary>;
	listMergeUnits(request: MergeUnitListRequest): Promise<MergeUnitPage>;
	getMergeUnit(analysisId: string, unitId: string): Promise<MergeUnitDetail>;
	getPluginOverview(playset: string | null): Promise<DesktopPluginOverview>;
	installPlugin(spec: string, sha256: string | null): Promise<DesktopPluginInstall>;
	importPluginDir(path: string): Promise<DesktopPluginInstall>;
	setPluginEnabled(
		playset: string | null,
		id: string,
		enabled: boolean,
		version: string | null,
	): Promise<void>;
	updatePluginConfig(
		playset: string | null,
		id: string,
		key: string,
		value: boolean | number | string,
	): Promise<void>;
	launchEu4WithPlugins(
		playset: string | null,
		prepareOnly: boolean,
	): Promise<DesktopLaunchResult>;
	getLaunchStatus(runDir: string): Promise<DesktopLaunchStatus>;
	getDesktopSettings(): Promise<DesktopPreferences>;
	saveDesktopSettings(settings: DesktopSettingsInput): Promise<DesktopPreferences>;
	commitMergeAnalysis(
		analysisId: string,
		outputDir: string,
	): Promise<MergeCommitResult>;
	testDownloadConnectivity(
		settings: DesktopSettingsInput | null,
	): Promise<DesktopConnectivityReport>;
}

export type InvokeCommand = <T>(
	command: string,
	args?: Record<string, unknown>,
) => Promise<T>;

export function createDesktopClient(invokeCommand: InvokeCommand): DesktopClient {
	return {
		inspectInput: (): Promise<InputInspection> => invokeCommand("inspect_input"),
		startMergeAnalysis: (
			inspectionId: string,
			inputMode: AnalysisInputMode,
		): Promise<StartMergeAnalysisResult> =>
			invokeCommand("start_merge_analysis", { inspectionId, inputMode }),
		cancelMergeAnalysis: (analysisId: string): Promise<void> =>
			invokeCommand("cancel_merge_analysis", { analysisId }),
		getMergeAnalysisSummary: (analysisId: string): Promise<MergeAnalysisSummary> =>
			invokeCommand("get_merge_analysis_summary", { analysisId }),
		listMergeUnits: (request: MergeUnitListRequest): Promise<MergeUnitPage> =>
			invokeCommand("list_merge_units", {
				analysisId: request.analysisId,
				query: request.query,
				disposition: request.disposition,
				page: request.page,
				pageSize: request.pageSize,
			}),
		getMergeUnit: (analysisId: string, unitId: string): Promise<MergeUnitDetail> =>
			invokeCommand("get_merge_unit", { analysisId, unitId }),
		getPluginOverview: (playset: string | null): Promise<DesktopPluginOverview> =>
			invokeCommand("get_plugin_overview", { playset }),
		installPlugin: (
			spec: string,
			sha256: string | null,
		): Promise<DesktopPluginInstall> =>
			invokeCommand("install_plugin", { spec, sha256 }),
		importPluginDir: (path: string): Promise<DesktopPluginInstall> =>
			invokeCommand("import_plugin_dir", { path }),
		setPluginEnabled: (
			playset: string | null,
			id: string,
			enabled: boolean,
			version: string | null,
		): Promise<void> =>
			invokeCommand("set_plugin_enabled", { playset, id, enabled, version }),
		updatePluginConfig: (
			playset: string | null,
			id: string,
			key: string,
			value: boolean | number | string,
		): Promise<void> =>
			invokeCommand("update_plugin_config", { playset, id, key, value }),
		launchEu4WithPlugins: (
			playset: string | null,
			prepareOnly: boolean,
		): Promise<DesktopLaunchResult> =>
			invokeCommand("launch_eu4_with_plugins", { playset, prepareOnly }),
		getLaunchStatus: (runDir: string): Promise<DesktopLaunchStatus> =>
			invokeCommand("get_launch_status", { runDir }),
		getDesktopSettings: (): Promise<DesktopPreferences> =>
			invokeCommand("get_desktop_settings"),
		saveDesktopSettings: (
			settings: DesktopSettingsInput,
		): Promise<DesktopPreferences> =>
			invokeCommand("save_desktop_settings", { settings }),
		commitMergeAnalysis: (
			analysisId: string,
			outputDir: string,
		): Promise<MergeCommitResult> =>
			invokeCommand("commit_merge_analysis", { analysisId, outputDir }),
		testDownloadConnectivity: (
			settings: DesktopSettingsInput | null,
		): Promise<DesktopConnectivityReport> =>
			invokeCommand("test_download_connectivity", { settings }),
	};
}

const tauriInvoke: InvokeCommand = <T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> => invoke<T>(command, args);

export const tauriDesktopClient: DesktopClient = createDesktopClient(tauriInvoke);

export function parseDesktopCommandError(error: unknown): DesktopCommandError {
	if (typeof error === "object" && error !== null) {
		const candidate = error as { code?: unknown; message?: unknown };
		if (typeof candidate.message === "string") {
			return {
				code: typeof candidate.code === "string" ? candidate.code : null,
				message: candidate.message,
			};
		}
	}

	return {
		code: null,
		message: error instanceof Error ? error.message : String(error),
	};
}

export function isAnalysisActive(state: MergeAnalysisState): boolean {
	return state === "queued" || state === "running";
}

export function hasMergeReview(state: MergeAnalysisState): boolean {
	return state === "ready" || state === "ready_with_deferrals" || state === "blocked";
}
