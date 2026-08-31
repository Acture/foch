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
export type ReadinessState = "ready" | "blocked";

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

export interface InputInspection {
	readiness: ReadinessState;
	game: InstalledGameView;
	baseData: BaseDataView;
	playset: DetectedPlaysetView | null;
	issues: ReadinessIssue[];
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

export interface DesktopClient {
	inspectInput(): Promise<InputInspection>;
	startMergeAnalysis(): Promise<StartMergeAnalysisResult>;
	cancelMergeAnalysis(analysisId: string): Promise<void>;
	getMergeAnalysisSummary(analysisId: string): Promise<MergeAnalysisSummary>;
	listMergeUnits(request: MergeUnitListRequest): Promise<MergeUnitPage>;
	getMergeUnit(analysisId: string, unitId: string): Promise<MergeUnitDetail>;
}

export type InvokeCommand = <T>(
	command: string,
	args?: Record<string, unknown>,
) => Promise<T>;

export function createDesktopClient(invokeCommand: InvokeCommand): DesktopClient {
	return {
		inspectInput: (): Promise<InputInspection> => invokeCommand("inspect_input"),
		startMergeAnalysis: (): Promise<StartMergeAnalysisResult> =>
			invokeCommand("start_merge_analysis"),
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
	};
}

const tauriInvoke: InvokeCommand = <T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> => invoke<T>(command, args);

export const tauriDesktopClient: DesktopClient = createDesktopClient(tauriInvoke);

export function isAnalysisActive(state: MergeAnalysisState): boolean {
	return state === "queued" || state === "running";
}
