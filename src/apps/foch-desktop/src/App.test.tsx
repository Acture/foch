import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type {
	AnalysisInputScope,
	DesktopClient,
	DesktopConnectivityReport,
	DesktopLaunchResult,
	DesktopLaunchStatus,
	DesktopPluginOverview,
	DesktopPreferences,
	DesktopResolutionRule,
	DesktopSettingsInput,
	InputInspection,
	InstallRecoveryNote,
	MergeAnalysisState,
	MergeAnalysisSummary,
	MergeCommitResult,
	MergeUnitDetail,
	MergeUnitPage,
	SetupStatus,
	StartMergeAnalysisResult,
	VerifiedGamePath,
} from "./api";

const COMPLETE_INPUT_SCOPE: AnalysisInputScope = {
	mode: "complete",
	sourceModCount: 3,
	omittedMods: [],
	omittedModCount: 0,
	includedModCount: 3,
};

const INCOMPLETE_INPUT_SCOPE: AnalysisInputScope = {
	mode: "without_unavailable_mods",
	sourceModCount: 3,
	omittedMods: [
		{
			id: "mod-2",
			name: "Trade Goods Expanded",
			position: 2,
			reason: "Workshop item 282001 is not installed",
		},
	],
	omittedModCount: 1,
	includedModCount: 2,
};

const READY_INSPECTION: InputInspection = {
	inspectionId: "inspection-ready",
	readiness: "ready",
	game: {
		name: "Europa Universalis IV",
		version: "1.37.5",
		installPath:
			"C:\\Program Files (x86)\\Steam\\steamapps\\common\\Europa Universalis IV",
	},
	baseData: {
		state: "ready",
		version: "1.37.5",
		detail: "Verified base snapshot",
	},
	playset: {
		name: "Campaign 1444",
		sourcePath:
			"C:\\Users\\Player\\Documents\\Paradox Interactive\\Europa Universalis IV\\dlc_load.json",
		mods: [
			{
				id: "mod-1",
				name: "Europa Expanded",
				position: 1,
				enabled: true,
				workshopId: "281990",
				workshopManifestId: "998877",
				version: "4.2.1",
				declaredDependencies: [],
				descriptorPath: "C:\\workshop\\281990\\descriptor.mod",
				sourceError: null,
			},
			{
				id: "mod-2",
				name: "Trade Goods Expanded",
				position: 2,
				enabled: true,
				workshopId: "282001",
				workshopManifestId: "998900",
				version: "2.0.0",
				declaredDependencies: ["Europa Expanded"],
				descriptorPath: "C:\\workshop\\282001\\descriptor.mod",
				sourceError: null,
			},
			{
				id: "mod-3",
				name: "Local Overrides",
				position: 3,
				enabled: false,
				workshopId: null,
				workshopManifestId: null,
				version: null,
				declaredDependencies: [],
				descriptorPath:
					"C:\\Users\\Player\\Documents\\Paradox Interactive\\Europa Universalis IV\\mod\\local.mod",
				sourceError: null,
			},
		],
	},
	issues: [],
	recovery: null,
};

const BLOCKED_INSPECTION: InputInspection = {
	...READY_INSPECTION,
	inspectionId: "inspection-blocked",
	readiness: "blocked",
	baseData: {
		state: "missing",
		version: null,
		detail: "Base data has not been built",
	},
	issues: [
		{
			id: "base-data",
			title: "EU4 base data is missing",
			detail: "Build the verified base snapshot before analysis.",
			action: "Run foch data build eu4",
		},
	],
};

const RECOVERABLE_INSPECTION: InputInspection = {
	...READY_INSPECTION,
	inspectionId: "inspection-recoverable",
	readiness: "ready_with_omissions",
	playset: {
		...READY_INSPECTION.playset!,
		mods: READY_INSPECTION.playset!.mods.map((mod) =>
			mod.id === "mod-2"
				? {
						...mod,
						workshopManifestId: null,
						version: null,
						declaredDependencies: [],
						descriptorPath: null,
						sourceError: "Workshop item 282001 is not installed",
					}
				: mod,
		),
	},
	issues: [
		{
			id: "workshop-mod-2",
			title: "Workshop mod 282001 is not ready",
			detail: "Workshop item 282001 is not installed",
			action: "Repair or redownload this Workshop item in Steam.",
		},
	],
	recovery: {
		kind: "without_unavailable_mods",
		sourceModCount: INCOMPLETE_INPUT_SCOPE.sourceModCount,
		omittedMods: INCOMPLETE_INPUT_SCOPE.omittedMods,
		omittedModCount: INCOMPLETE_INPUT_SCOPE.omittedModCount,
		includedModCount: INCOMPLETE_INPUT_SCOPE.includedModCount,
	},
};

const UNIT_PAGE: MergeUnitPage = {
	items: [
		{
			id: "safe-unit",
			path: "common/countries/France.txt",
			family: "countries",
			kind: "file",
			disposition: "safe",
			strategy: "gumtree-pcs-nway",
			contributorCount: 2,
		},
		{
			id: "copy-unit",
			path: "gfx/flags/FRA.tga",
			family: "binary_assets",
			kind: "file",
			disposition: "copy",
			strategy: "copy",
			contributorCount: 1,
		},
		{
			id: "choice-unit",
			path: "missions/french_missions.txt",
			family: "missions",
			kind: "definition_module",
			disposition: "needs_user_choice",
			strategy: "gumtree-pcs-nway",
			contributorCount: 3,
		},
		{
			id: "unsupported-unit",
			path: "map/terrain.bmp",
			family: "map",
			kind: "file",
			disposition: "unsupported_input",
			strategy: "unsupported",
			contributorCount: 2,
		},
		{
			id: "failure-unit",
			path: "common/ideas/broken.txt",
			family: "ideas",
			kind: "file",
			disposition: "engine_failure",
			strategy: "gumtree-pcs-nway",
			contributorCount: 2,
		},
		{
			id: "deferred-unit",
			path: "events/review_later.txt",
			family: "events",
			kind: "file",
			disposition: "deferred",
			strategy: "gumtree-pcs-nway",
			contributorCount: 2,
		},
	],
	total: 6,
	page: 1,
	pageSize: 12,
};

const UNIT_DETAIL: MergeUnitDetail = {
	id: "safe-unit",
	path: "common/countries/France.txt",
	family: "countries",
	kind: "file",
	disposition: "safe",
	strategy: "gumtree-pcs-nway",
	summary: "Both contributions can be preserved without a choice.",
	outputPath: "common/countries/France.txt",
	outputPaths: ["common/countries/France.txt"],
	contributors: [
		{ modId: "base", name: "Europa Universalis IV", position: 0, isBaseGame: true },
		{ modId: "mod-1", name: "Europa Expanded", position: 1, isBaseGame: false },
	],
	notes: ["Base snapshot used as the semantic ancestor."],
	conflicts: [],
};

const PREFER_B_FILE_RULE: DesktopResolutionRule = {
	matchPattern: null,
	file: "history/countries/TES - Test.txt",
	preferMod: "ugc-300002",
	handler: null,
};

const PREFER_B_FOLDER_RULE: DesktopResolutionRule = {
	matchPattern: "re:^history/countries/[^/]+$",
	file: null,
	preferMod: "ugc-300002",
	handler: null,
};

const CONFLICT_UNIT_DETAIL: MergeUnitDetail = {
	id: "choice-unit",
	path: "history/countries/TES - Test.txt",
	family: "countries",
	kind: "file",
	disposition: "needs_user_choice",
	strategy: "structural_merge",
	summary: "Three mods set the same country value differently.",
	outputPath: null,
	outputPaths: [],
	contributors: [
		{ modId: "ugc-300000", name: "Baseline", position: 0, isBaseGame: false },
		{ modId: "ugc-300001", name: "Conflict A", position: 1, isBaseGame: false },
		{ modId: "ugc-300002", name: "Conflict B", position: 2, isBaseGame: false },
	],
	notes: [],
	conflicts: [
		{
			filePath: "history/countries/TES - Test.txt",
			reason: "manual conflict",
			leaves: [
				{
					conflictId: "ab12cd34",
					address: "religion",
					kind: null,
					reason: "three mods set religion differently",
					vanillaSnippet: "religion = catholic",
					candidates: [
						{
							modId: "ugc-300000",
							modDisplayName: "Baseline",
							precedence: 0,
							changeSummary: ["set religion"],
							rendered: "religion = catholic",
						},
						{
							modId: "ugc-300001",
							modDisplayName: "Conflict A",
							precedence: 1,
							changeSummary: ["set religion"],
							rendered: "religion = orthodox",
						},
						{
							modId: "ugc-300002",
							modDisplayName: "Conflict B",
							precedence: 2,
							changeSummary: ["set religion"],
							rendered: "religion = protestant",
						},
					],
					options: [
						{
							action: { kind: "prefer_mod", modId: "ugc-300002" },
							fileResolution: PREFER_B_FILE_RULE,
							directoryResolution: PREFER_B_FOLDER_RULE,
						},
						{
							action: { kind: "handler", name: "last_writer" },
							fileResolution: {
								matchPattern: "re:^history/countries/TES - Test\\.txt$",
								file: null,
								preferMod: null,
								handler: "last_writer",
							},
							directoryResolution: {
								matchPattern: "re:^history/countries/[^/]+$",
								file: null,
								preferMod: null,
								handler: "last_writer",
							},
						},
					],
				},
			],
		},
	],
};

function analysisSummary(state: MergeAnalysisState): MergeAnalysisSummary {
	const active: boolean = state === "queued" || state === "running";
	return {
		analysisId: "analysis-1",
		state,
		stage: active ? "semantic_merge" : "complete",
		completedUnits: active ? 7 : 20,
		totalUnits: 20,
		elapsedMs: active ? 8_000 : 92_000,
		counts: {
			total: 20,
			safe: 8,
			copy: 4,
			needsUserChoice: 3,
			unsupportedInput: 2,
			engineFailure: 1,
			deferred: 2,
		},
		message:
			state === "ready_with_deferrals"
				? "Three units require review before export."
				: null,
	};
}

const ENGLISH_SETTINGS: DesktopPreferences = {
	transport: "auto",
	mirror: "auto",
	customMirrors: [],
	language: "auto",
	effectiveLanguage: "en",
};

const EMPTY_OVERVIEW: DesktopPluginOverview = {
	playset: "Campaign 1444",
	gameRoot: READY_INSPECTION.game.installPath,
	gameVersion: "1.37.5",
	hostDllAvailable: true,
	hostDllDetail: "ready (foch_runtime_host.dll)",
	entries: [],
	companionWarnings: [],
	plan: { launchable: true, order: [], errors: [], warnings: [] },
	preferences: ENGLISH_SETTINGS,
	problems: [],
};

const PLUGIN_OVERVIEW: DesktopPluginOverview = {
	...EMPTY_OVERVIEW,
	companionWarnings: [
		"Europa Expanded declares dev.foch.sample, which is not installed",
	],
	entries: [
		{
			id: "dev.foch.sample",
			name: "Sample",
			description: "Declared by the playset",
			installedVersions: [],
			selectedVersion: null,
			enabled: false,
			installSpec: "Acture/sample",
			declaredByMods: ["Europa Expanded"],
			requiredByMods: ["Europa Expanded"],
			conflicts: [],
			configFields: [],
			badge: "not_installed",
		},
		{
			id: "dev.foch.slider",
			name: "Slider",
			description: "Tunable limit",
			installedVersions: ["0.1.0"],
			selectedVersion: "0.1.0",
			enabled: true,
			installSpec: null,
			declaredByMods: [],
			requiredByMods: [],
			conflicts: ["dev.foch.other (overlapping hooks)"],
			configFields: [
				{
					key: "limit",
					kind: "int",
					description: "Limit",
					value: 1,
					min: 0,
					max: 8,
				},
			],
			badge: "enabled",
		},
	],
	plan: {
		launchable: true,
		order: ["dev.foch.slider 0.1.0"],
		errors: [],
		warnings: [],
	},
};

function emptySetup(inspection: InputInspection | null = null): SetupStatus {
	return {
		phase: "idle",
		stage: null,
		completed: null,
		total: null,
		stages: [],
		inspection,
		message: null,
		buildId: null,
		buildStage: null,
		buildCompleted: null,
		buildTotal: null,
		documentCompleted: null,
		documentTotal: null,
		installNote: null,
	};
}

function installNoteFor(inspection: InputInspection): InstallRecoveryNote | null {
	if (inspection.baseData.state === "ready") return null;
	const version: string =
		inspection.baseData.version ?? inspection.game.version ?? "unknown";
	const path: string = inspection.game.installPath ?? "<EU4 installation directory>";
	return {
		version,
		rulesVersion: "rules-test",
		reason:
			"Remote foch data install is not available for this EU4 version and rules. Releases do not ship a required base-data asset, so a remote EU4 data asset is not a prerequisite.",
		localCommand: `foch data build eu4 --from-game-path "${path}" --game-version ${version} --install`,
	};
}

function setupFromInspection(inspection: InputInspection): SetupStatus {
	const modCount: number = inspection.playset?.mods.length ?? 0;
	return {
		...emptySetup(inspection),
		phase: "checked",
		stage: "base_data",
		stages: [
			{ stage: "game_install", completed: null, total: null },
			{ stage: "launcher_input", completed: null, total: null },
			{ stage: "workshop_acf", completed: null, total: null },
			{ stage: "descriptors", completed: modCount, total: modCount },
			{ stage: "base_data", completed: null, total: null },
		],
		installNote: installNoteFor(inspection),
	};
}

function createClient(overrides: Partial<DesktopClient> = {}): DesktopClient {
	const client: DesktopClient = {
		inspectInput: vi.fn(async (): Promise<InputInspection> => READY_INSPECTION),
		getPluginOverview: vi.fn(
			async (): Promise<DesktopPluginOverview> => EMPTY_OVERVIEW,
		),
		installPlugin: vi.fn(async () => ({
			id: "dev.foch.sample",
			version: "0.1.0",
			source: "Acture/sample@v0.1.0 via http (direct)",
			conflicts: [],
			alreadyPresent: false,
		})),
		importPluginDir: vi.fn(async () => ({
			id: "dev.foch.local",
			version: "0.1.0",
			source: "local directory",
			conflicts: [],
			alreadyPresent: false,
		})),
		setPluginEnabled: vi.fn(async (): Promise<void> => undefined),
		updatePluginConfig: vi.fn(async (): Promise<void> => undefined),
		launchEu4WithPlugins: vi.fn(async (): Promise<DesktopLaunchResult> => ({
			runDir: "C:\\runtime\\prepared",
			planSha256: "abc",
			preparedOnly: true,
			pid: null,
			pluginCount: 1,
			versionDll: true,
		})),
		getLaunchStatus: vi.fn(async (): Promise<DesktopLaunchStatus> => ({
			runId: "p1",
			plugins: [{ id: "dev.foch.slider", state: "active", reason: null }],
		})),
		getDesktopSettings: vi.fn(
			async (): Promise<DesktopPreferences> => ENGLISH_SETTINGS,
		),
		saveDesktopSettings: vi.fn(
			async (settings: DesktopSettingsInput): Promise<DesktopPreferences> => ({
				...settings,
				effectiveLanguage: settings.language === "zh_cn" ? "zh-CN" : "en",
			}),
		),
		startMergeAnalysis: vi.fn(async (): Promise<StartMergeAnalysisResult> => ({
			analysisId: "analysis-1",
			inputScope: COMPLETE_INPUT_SCOPE,
		})),
		cancelMergeAnalysis: vi.fn(async (): Promise<void> => undefined),
		getMergeAnalysisSummary: vi.fn(async (): Promise<MergeAnalysisSummary> =>
			analysisSummary("ready_with_deferrals"),
		),
		listMergeUnits: vi.fn(async (): Promise<MergeUnitPage> => UNIT_PAGE),
		getMergeUnit: vi.fn(async (): Promise<MergeUnitDetail> => UNIT_DETAIL),
		renderResolutionsToml: vi.fn(
			async (rules: DesktopResolutionRule[]): Promise<string> =>
				`# ${rules.length} rule(s)\n[[resolutions]]\nfile = "history/countries/TES - Test.txt"\nprefer_mod = "ugc-300002"\n`,
		),
		commitMergeAnalysis: vi.fn(
			async (analysisId: string, outputDir: string): Promise<MergeCommitResult> => ({
				analysisId,
				outputDir,
				mergeStatus: "partial_success",
				generatedFileCount: 4,
				manualConflictCount: 1,
				unsupportedInputCount: 0,
				engineFailureCount: 0,
				committedAtNote: "english note",
			}),
		),
		testDownloadConnectivity: vi.fn(async (): Promise<DesktopConnectivityReport> => ({
			transport: "auto",
			mirrorPolicy: "auto",
			routes: [
				{
					label: "direct",
					url: "https://github.com/Acture/foch",
					kind: "direct",
					reachable: true,
					httpStatus: 200,
					latencyMs: 12,
					detail: "HEAD",
				},
			],
		})),
		startSetupCheck: vi.fn(async (): Promise<SetupStatus> => emptySetup()),
		getSetupStatus: vi.fn(async (): Promise<SetupStatus> => emptySetup()),
		verifyGamePath: vi.fn(async (path: string): Promise<VerifiedGamePath> => ({
			version: "1.37.5",
			installPath: path,
		})),
		saveGamePath: vi.fn(async (path: string): Promise<VerifiedGamePath> => ({
			version: "1.37.5",
			installPath: path,
		})),
		startBaseDataBuild: vi.fn(async (): Promise<SetupStatus> => ({
			...emptySetup(),
			phase: "building",
			stage: "detect_version",
			buildId: "build-1",
			buildStage: "detect_version",
		})),
		cancelBaseDataBuild: vi.fn(async (): Promise<SetupStatus> => ({
			...emptySetup(),
			phase: "build_cancelled",
			message:
				"The base data build was cancelled. A partial snapshot was not installed.",
		})),
		...overrides,
	};
	if (overrides.startSetupCheck === undefined) {
		let latest: SetupStatus = {
			...emptySetup(),
			phase: "checking",
			stage: "game_install",
			stages: [{ stage: "game_install", completed: null, total: null }],
		};
		const inspect = client.inspectInput;
		client.startSetupCheck = vi.fn(async (): Promise<SetupStatus> => {
			latest = {
				...latest,
				phase: "checking",
				stage: "game_install",
				completed: null,
				total: null,
			};
			const inspection = await inspect();
			latest = setupFromInspection(inspection);
			return latest;
		});
		if (overrides.getSetupStatus === undefined) {
			client.getSetupStatus = vi.fn(async (): Promise<SetupStatus> => latest);
		}
	}
	return client;
}

function renderMerge(client: DesktopClient, pollIntervalMs = 900) {
	const view = render(<App client={client} pollIntervalMs={pollIntervalMs} />);
	fireEvent.click(screen.getByRole("tab", { name: /Merge & Conflicts/ }));
	return view;
}

function deferred<T>(): {
	promise: Promise<T>;
	resolve: (value: T) => void;
} {
	let resolvePromise: (value: T) => void = (): void => undefined;
	const promise = new Promise<T>((resolve): void => {
		resolvePromise = resolve;
	});
	return { promise, resolve: resolvePromise };
}

describe("Foch desktop analysis checkpoint", (): void => {
	afterEach((): void => {
		vi.useRealTimers();
	});

	it("shows first-launch loading, then the detected playset and source identity", async (): Promise<void> => {
		const pendingInspection = deferred<InputInspection>();
		const client = createClient({
			inspectInput: vi.fn((): Promise<InputInspection> => pendingInspection.promise),
		});

		renderMerge(client);
		expect(
			screen.getByRole("heading", { name: "Checking EU4 setup" }),
		).toBeInTheDocument();

		await act(async (): Promise<void> => {
			pendingInspection.resolve(READY_INSPECTION);
		});

		expect(await screen.findByText("Ready to analyze")).toBeInTheDocument();
		expect(screen.getByText("ready · 1.37.5")).toHaveAttribute(
			"title",
			"Verified base snapshot",
		);
		expect(
			screen.getByText("Workshop 281990 · v4.2.1 · manifest 998877"),
		).toBeInTheDocument();
		expect(screen.getByText("Depends on: Europa Expanded")).toBeInTheDocument();
		expect(
			screen.getByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
	});

	it("keeps analysis blocked until readiness issues are resolved", async (): Promise<void> => {
		const inspectInput = vi
			.fn<DesktopClient["inspectInput"]>()
			.mockResolvedValueOnce(BLOCKED_INSPECTION)
			.mockResolvedValueOnce(READY_INSPECTION);
		const client = createClient({ inspectInput });

		renderMerge(client);

		expect(
			await screen.findByRole("heading", {
				name: "Resolve setup blockers before analysis",
			}),
		).toBeInTheDocument();
		expect(screen.getByText("EU4 base data is missing")).toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: /Analyze current playset/i }),
		).not.toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: /Analyze available mods/i }),
		).not.toBeInTheDocument();

		fireEvent.click(screen.getByRole("button", { name: "Check again" }));
		expect(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
		expect(inspectInput).toHaveBeenCalledTimes(2);
	});

	it("shows unavailable mods and retries without changing the Launcher playset", async (): Promise<void> => {
		const inspectInput = vi
			.fn<DesktopClient["inspectInput"]>()
			.mockResolvedValueOnce(RECOVERABLE_INSPECTION)
			.mockResolvedValueOnce(READY_INSPECTION);
		const startMergeAnalysis = vi.fn<DesktopClient["startMergeAnalysis"]>();
		const client = createClient({ inspectInput, startMergeAnalysis });

		renderMerge(client);

		expect(
			await screen.findByRole("heading", { name: "1 Workshop mod unavailable" }),
		).toBeInTheDocument();
		expect(
			screen.getByText("Available mods ready").closest(".readiness-seal"),
		).toHaveClass("readiness-ready_with_omissions");
		expect(
			screen.getByText("Unavailable", { selector: ".mod-state" }),
		).toBeInTheDocument();
		expect(
			screen.getByText("Unavailable: Workshop item 282001 is not installed"),
		).toBeInTheDocument();
		expect(
			screen.getByText(/The Launcher playset and every mod file stay untouched/),
		).toBeInTheDocument();
		expect(
			screen.getByText("Ignore missing items for this session only."),
		).toBeInTheDocument();
		expect(screen.getByText("Results will be marked incomplete")).toBeInTheDocument();

		fireEvent.click(screen.getByRole("button", { name: "Check again" }));

		expect(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
		expect(inspectInput).toHaveBeenCalledTimes(2);
		expect(startMergeAnalysis).not.toHaveBeenCalled();
	});

	it("shows stage status without a percentage until a real count exists", async (): Promise<void> => {
		let status: SetupStatus = {
			...emptySetup(),
			phase: "checking",
			stage: "game_install",
			stages: [{ stage: "game_install", completed: null, total: null }],
		};
		const client = createClient({
			startSetupCheck: vi.fn(async (): Promise<SetupStatus> => status),
			getSetupStatus: vi.fn(async (): Promise<SetupStatus> => status),
		});
		renderMerge(client, 20);

		expect(await screen.findByText("Reading the EU4 installation")).toBeInTheDocument();
		expect(screen.getByText("This stage has no item count yet.")).toBeInTheDocument();
		expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
		expect(screen.queryByText(/%/)).not.toBeInTheDocument();

		status = {
			...status,
			stage: "descriptors",
			completed: 1,
			total: 2,
			stages: [{ stage: "descriptors", completed: 1, total: 2 }],
		};
		expect(await screen.findByRole("progressbar")).toBeInTheDocument();
		expect(screen.getByText("1 / 2")).toBeInTheDocument();
		expect(screen.getByText("Reading mod descriptors")).toBeInTheDocument();
	});

	it("verifies a chosen EU4 path before saving it", async (): Promise<void> => {
		const missingPath: InputInspection = {
			...BLOCKED_INSPECTION,
			game: { ...BLOCKED_INSPECTION.game, version: null, installPath: null },
			issues: [
				{
					id: "eu4_install_unavailable",
					title: "Europa Universalis IV is not ready",
					detail: "EU4 was not found.",
					action: "Select the EU4 installation directory.",
				},
			],
		};
		const verifyGamePath = vi.fn(async (path: string): Promise<VerifiedGamePath> => {
			if (path.includes("missing")) {
				throw {
					code: "game_path_invalid",
					message: "EU4 version was not found. Nothing was written.",
				};
			}
			return { version: "1.37.5", installPath: path };
		});
		const saveGamePath = vi.fn(async (path: string): Promise<VerifiedGamePath> => ({
			version: "1.37.5",
			installPath: path,
		}));
		const client = createClient({
			inspectInput: vi.fn(async (): Promise<InputInspection> => missingPath),
			verifyGamePath,
			saveGamePath,
		});
		renderMerge(client);

		const field = await screen.findByLabelText("EU4 installation directory");
		fireEvent.change(field, { target: { value: "D:\\missing" } });
		fireEvent.click(screen.getByRole("button", { name: "Verify path" }));
		expect(await screen.findByRole("alert")).toHaveTextContent("Nothing was written.");
		expect(saveGamePath).not.toHaveBeenCalled();

		fireEvent.change(field, { target: { value: "D:\\EU4" } });
		fireEvent.click(screen.getByRole("button", { name: "Verify path" }));
		expect(await screen.findByText("Verified version 1.37.5")).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Use this EU4 install" }));
		await waitFor((): void => {
			expect(saveGamePath).toHaveBeenCalledWith("D:\\EU4");
		});
	});

	it("shows a path error when saving the verified EU4 install fails", async (): Promise<void> => {
		const missingPath: InputInspection = {
			...BLOCKED_INSPECTION,
			game: { ...BLOCKED_INSPECTION.game, version: null, installPath: null },
			issues: [
				{
					id: "eu4_install_unavailable",
					title: "Europa Universalis IV is not ready",
					detail: "EU4 was not found.",
					action: "Select the EU4 installation directory.",
				},
			],
		};
		const saveGamePath = vi.fn(async (): Promise<VerifiedGamePath> => {
			throw {
				code: "config_unavailable",
				message: "Foch could not open its configuration directory",
			};
		});
		const client = createClient({
			inspectInput: vi.fn(async (): Promise<InputInspection> => missingPath),
			saveGamePath,
		});
		renderMerge(client);

		const field = await screen.findByLabelText("EU4 installation directory");
		fireEvent.change(field, { target: { value: "D:\\EU4" } });
		fireEvent.click(screen.getByRole("button", { name: "Verify path" }));
		expect(await screen.findByText("Verified version 1.37.5")).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Use this EU4 install" }));
		expect(await screen.findByRole("alert")).toHaveTextContent(
			"could not open its configuration directory",
		);
		expect(saveGamePath).toHaveBeenCalledWith("D:\\EU4");
	});

	it("explains why remote data install is not required and builds with real counts", async (): Promise<void> => {
		let status: SetupStatus = setupFromInspection(BLOCKED_INSPECTION);
		const startBaseDataBuild = vi.fn(async (): Promise<SetupStatus> => {
			status = {
				...status,
				phase: "building",
				stage: "parse_documents",
				completed: 1,
				total: 2,
				buildId: "build-7",
				buildStage: "parse_documents",
				buildCompleted: 1,
				buildTotal: 2,
				documentCompleted: 1,
				documentTotal: 2,
			};
			return status;
		});
		const cancelBaseDataBuild = vi.fn(async (): Promise<SetupStatus> => {
			status = {
				...status,
				phase: "build_cancelled",
				message:
					"The base data build was cancelled. A partial snapshot was not installed.",
				buildId: null,
			};
			return status;
		});
		const client = createClient({
			inspectInput: vi.fn(async (): Promise<InputInspection> => BLOCKED_INSPECTION),
			startBaseDataBuild,
			cancelBaseDataBuild,
			getSetupStatus: vi.fn(async (): Promise<SetupStatus> => status),
		});
		renderMerge(client, 20);

		expect(await screen.findByText(/not a prerequisite/)).toBeInTheDocument();
		expect(screen.getByText(/--from-game-path/)).toBeInTheDocument();
		expect(screen.getByText(/rules-test/)).toBeInTheDocument();
		fireEvent.click(
			screen.getByRole("button", { name: "Build base data from this install" }),
		);
		expect(await screen.findByRole("progressbar")).toBeInTheDocument();
		expect(screen.getByText("1 / 2")).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Cancel base data build" }));
		expect(
			await screen.findByText(/partial snapshot was not installed/),
		).toBeInTheDocument();
		expect(startBaseDataBuild).toHaveBeenCalledTimes(1);
		expect(cancelBaseDataBuild).toHaveBeenCalledWith("build-7");
	});

	it("ignores a second click while a base data build is starting", async (): Promise<void> => {
		const pending = deferred<SetupStatus>();
		const startBaseDataBuild = vi.fn((): Promise<SetupStatus> => pending.promise);
		const client = createClient({
			inspectInput: vi.fn(async (): Promise<InputInspection> => BLOCKED_INSPECTION),
			startBaseDataBuild,
		});
		renderMerge(client);
		const button = await screen.findByRole("button", {
			name: "Build base data from this install",
		});
		fireEvent.click(button);
		fireEvent.click(button);
		expect(startBaseDataBuild).toHaveBeenCalledTimes(1);
		await act(async (): Promise<void> => {
			pending.resolve({
				...setupFromInspection(BLOCKED_INSPECTION),
				phase: "building",
				buildId: "build-2",
				buildStage: "detect_version",
			});
		});
	});

	it("starts the frozen available-mod scope and labels every analysis state incomplete", async (): Promise<void> => {
		const pendingSummary = deferred<MergeAnalysisSummary>();
		const startMergeAnalysis = vi.fn<DesktopClient["startMergeAnalysis"]>(
			async (): Promise<StartMergeAnalysisResult> => ({
				analysisId: "analysis-incomplete",
				inputScope: INCOMPLETE_INPUT_SCOPE,
			}),
		);
		const client = createClient({
			inspectInput: vi.fn(async (): Promise<InputInspection> => RECOVERABLE_INSPECTION),
			startMergeAnalysis,
			getMergeAnalysisSummary: vi.fn(() => pendingSummary.promise),
		});
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze available mods/i }),
		);

		expect(await screen.findByRole("heading", { name: "Queued" })).toBeInTheDocument();
		expect(startMergeAnalysis).toHaveBeenCalledWith(
			"inspection-recoverable",
			"without_unavailable_mods",
			[],
		);
		expect(
			screen.getByText("Incomplete input", { selector: ".section-kicker" }),
		).toBeInTheDocument();
		expect(screen.getByText("2 of 3 playset mods analyzed")).toBeInTheDocument();
		expect(
			screen.getByText(/the Launcher playset remains unchanged/i),
		).toBeInTheDocument();
		expect(
			screen.getByRole("heading", { name: "Building the available-mod review" }),
		).toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Building the complete review" }),
		).not.toBeInTheDocument();

		await act(async (): Promise<void> => {
			pendingSummary.resolve(analysisSummary("ready_with_deferrals"));
		});

		expect(
			await screen.findByRole("heading", { name: "Incomplete review required" }),
		).toBeInTheDocument();
		expect(screen.getByText("2 of 3 playset mods analyzed")).toBeInTheDocument();
		expect(await screen.findByText("6 results")).toBeInTheDocument();
	});

	it("lands on the review summary before opening focused unit detail", async (): Promise<void> => {
		const getMergeUnit = vi.fn<DesktopClient["getMergeUnit"]>(
			async (): Promise<MergeUnitDetail> => UNIT_DETAIL,
		);
		const client = createClient({ getMergeUnit });
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);

		expect(
			await screen.findByRole("heading", { name: "Review required" }),
		).toBeInTheDocument();
		expect(screen.getByText("20 / 20 units")).toBeInTheDocument();
		expect(screen.getByText("1m 32s elapsed")).toBeInTheDocument();
		for (const label of [
			"Safe merge",
			"Copy through",
			"Needs a choice",
			"Unsupported input",
			"Engine failure",
			"Deferred",
		]) {
			expect(screen.getAllByText(label).length).toBeGreaterThan(0);
		}
		expect(client.startMergeAnalysis).toHaveBeenCalledWith(
			"inspection-ready",
			"complete",
			[],
		);
		expect(
			screen.getByRole("heading", { name: "Select a merge unit" }),
		).toBeInTheDocument();
		expect(await screen.findByText("6 results")).toBeInTheDocument();
		expect(getMergeUnit).not.toHaveBeenCalled();

		fireEvent.click(
			screen.getByRole("button", { name: /common\/countries\/France\.txt/i }),
		);
		expect(
			await screen.findByText("Both contributions can be preserved without a choice."),
		).toBeInTheDocument();
		expect(getMergeUnit).toHaveBeenCalledWith("analysis-1", "safe-unit");
		expect(
			screen.getByText("Base snapshot used as the semantic ancestor."),
		).toBeInTheDocument();
	});

	it("waits for a complete review before querying bounded unit endpoints", async (): Promise<void> => {
		const listMergeUnits = vi.fn<DesktopClient["listMergeUnits"]>(
			async (): Promise<MergeUnitPage> => UNIT_PAGE,
		);
		const getMergeUnit = vi.fn<DesktopClient["getMergeUnit"]>(
			async (): Promise<MergeUnitDetail> => UNIT_DETAIL,
		);
		const client = createClient({
			getMergeAnalysisSummary: vi.fn(async (): Promise<MergeAnalysisSummary> =>
				analysisSummary("running"),
			),
			listMergeUnits,
			getMergeUnit,
		});
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);

		expect(
			await screen.findByRole("heading", { name: "Building the complete review" }),
		).toBeInTheDocument();
		await waitFor((): void => {
			expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "35");
		});
		const unitBrowser: Element | null = document.querySelector(".unit-browser");
		expect(unitBrowser).toBeInstanceOf(HTMLElement);
		if (!(unitBrowser instanceof HTMLElement))
			throw new Error("merge unit browser is missing");
		expect(window.getComputedStyle(unitBrowser).display).toBe("none");
		expect(listMergeUnits).not.toHaveBeenCalled();
		expect(getMergeUnit).not.toHaveBeenCalled();
	});

	it("enters queued state immediately after starting and prevents a duplicate run", async (): Promise<void> => {
		const pendingSummary = deferred<MergeAnalysisSummary>();
		const startMergeAnalysis = vi.fn<DesktopClient["startMergeAnalysis"]>(
			async (): Promise<StartMergeAnalysisResult> => ({
				analysisId: "analysis-queued",
				inputScope: COMPLETE_INPUT_SCOPE,
			}),
		);
		const client = createClient({
			startMergeAnalysis,
			getMergeAnalysisSummary: vi.fn(() => pendingSummary.promise),
		});
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);

		expect(await screen.findByRole("heading", { name: "Queued" })).toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: /Analyze current playset/i }),
		).not.toBeInTheDocument();
		expect(startMergeAnalysis).toHaveBeenCalledTimes(1);
	});

	it("backs off transient update failures, pauses after three, and retries explicitly", async (): Promise<void> => {
		const pollIntervalMs = 100;
		const getMergeAnalysisSummary = vi
			.fn<DesktopClient["getMergeAnalysisSummary"]>()
			.mockRejectedValueOnce({ code: "summary_unavailable", message: "temporary 1" })
			.mockRejectedValueOnce({ code: "summary_unavailable", message: "temporary 2" })
			.mockRejectedValueOnce({ code: "summary_unavailable", message: "temporary 3" })
			.mockResolvedValueOnce(analysisSummary("running"));
		const client = createClient({ getMergeAnalysisSummary });
		renderMerge(client, pollIntervalMs);
		const analyzeButton = await screen.findByRole("button", {
			name: /Analyze current playset/i,
		});

		vi.useFakeTimers();
		await act(async (): Promise<void> => {
			fireEvent.click(analyzeButton);
			await Promise.resolve();
		});

		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(1);
		expect(screen.getByRole("heading", { name: "Queued" })).toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Analysis failed" }),
		).not.toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: "Retry update" }),
		).not.toBeInTheDocument();

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(pollIntervalMs - 1);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(1);

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(1);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(2);

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(pollIntervalMs * 2 - 1);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(2);

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(1);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(3);
		expect(screen.getByRole("button", { name: "Retry update" })).toBeInTheDocument();
		expect(screen.getByText(/temporary 3/)).toBeInTheDocument();
		expect(screen.getByRole("heading", { name: "Queued" })).toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Analysis failed" }),
		).not.toBeInTheDocument();

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(pollIntervalMs * 10);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(3);

		await act(async (): Promise<void> => {
			fireEvent.click(screen.getByRole("button", { name: "Retry update" }));
			await Promise.resolve();
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(4);
		expect(screen.getByRole("heading", { name: "Analyzing" })).toBeInTheDocument();
		expect(screen.queryByText(/Analysis update failed/)).not.toBeInTheDocument();
	});

	it("stops polling on a structured analysis_not_found error without inventing failure", async (): Promise<void> => {
		const getMergeAnalysisSummary = vi
			.fn<DesktopClient["getMergeAnalysisSummary"]>()
			.mockRejectedValue({
				code: "analysis_not_found",
				message: "The analysis record expired",
			});
		const client = createClient({ getMergeAnalysisSummary });
		renderMerge(client, 100);
		const analyzeButton = await screen.findByRole("button", {
			name: /Analyze current playset/i,
		});

		vi.useFakeTimers();
		await act(async (): Promise<void> => {
			fireEvent.click(analyzeButton);
			await Promise.resolve();
		});

		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(1);
		expect(screen.getByText(/The analysis record expired/)).toBeInTheDocument();
		expect(screen.queryByText("[object Object]")).not.toBeInTheDocument();
		expect(
			screen.getByRole("heading", { name: "Analysis unavailable" }),
		).toBeInTheDocument();
		expect(
			screen.getByRole("button", { name: "Check input again" }),
		).toBeInTheDocument();
		expect(screen.queryByRole("heading", { name: "Queued" })).not.toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Analysis failed" }),
		).not.toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: "Retry update" }),
		).not.toBeInTheDocument();

		await act(async (): Promise<void> => {
			await vi.advanceTimersByTimeAsync(10_000);
		});
		expect(getMergeAnalysisSummary).toHaveBeenCalledTimes(1);

		await act(async (): Promise<void> => {
			fireEvent.click(screen.getByRole("button", { name: "Check input again" }));
			await Promise.resolve();
		});
		expect(client.inspectInput).toHaveBeenCalledTimes(2);
		expect(
			screen.getByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
	});

	it("passes search and outcome filters through the bounded list endpoint", async (): Promise<void> => {
		const listMergeUnits = vi.fn<DesktopClient["listMergeUnits"]>(
			async (): Promise<MergeUnitPage> => UNIT_PAGE,
		);
		const client = createClient({ listMergeUnits });
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		await screen.findByText("6 results");

		fireEvent.change(screen.getByPlaceholderText("Search path or family"), {
			target: { value: "missions" },
		});
		expect(screen.getByPlaceholderText("Search path or family")).toHaveAttribute(
			"maxlength",
			"256",
		);
		fireEvent.click(screen.getByRole("button", { name: "Search" }));

		await waitFor((): void => {
			expect(listMergeUnits).toHaveBeenCalledWith(
				expect.objectContaining({ query: "missions", disposition: null, page: 1 }),
			);
		});

		fireEvent.change(
			screen.getByRole("combobox", { name: "Filter merge units by outcome" }),
			{
				target: { value: "needs_user_choice" },
			},
		);
		await waitFor((): void => {
			expect(listMergeUnits).toHaveBeenCalledWith(
				expect.objectContaining({
					query: "missions",
					disposition: "needs_user_choice",
					page: 1,
				}),
			);
		});
	});

	it("uses server pagination metadata and stable unit ids for detail lookup", async (): Promise<void> => {
		const secondUnit: MergeUnitDetail = {
			...UNIT_DETAIL,
			id: "choice-unit",
			path: "missions/french_missions.txt",
			disposition: "needs_user_choice",
		};
		const secondPage: MergeUnitPage = {
			items: [UNIT_PAGE.items[2]],
			total: 13,
			page: 2,
			pageSize: 12,
		};
		const listMergeUnits = vi
			.fn<DesktopClient["listMergeUnits"]>()
			.mockResolvedValueOnce({ ...UNIT_PAGE, total: 13 })
			.mockResolvedValueOnce(secondPage);
		const getMergeUnit = vi.fn<DesktopClient["getMergeUnit"]>(
			async (_analysisId: string, unitId: string): Promise<MergeUnitDetail> =>
				unitId === secondUnit.id ? secondUnit : UNIT_DETAIL,
		);
		const client = createClient({ listMergeUnits, getMergeUnit });
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		expect(await screen.findByText("Page 1 of 2")).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Next" }));

		expect(await screen.findByText("Page 2 of 2")).toBeInTheDocument();
		await waitFor((): void => {
			expect(listMergeUnits).toHaveBeenLastCalledWith(
				expect.objectContaining({ page: 2, pageSize: 12 }),
			);
		});
		expect(getMergeUnit).not.toHaveBeenCalled();

		fireEvent.click(
			screen.getByRole("button", { name: /missions\/french_missions\.txt/i }),
		);
		await waitFor((): void => {
			expect(getMergeUnit).toHaveBeenCalledWith("analysis-1", "choice-unit");
		});
	});

	it("cancels an active analysis by opaque analysis id", async (): Promise<void> => {
		const cancelMergeAnalysis = vi.fn<DesktopClient["cancelMergeAnalysis"]>(
			async (): Promise<void> => undefined,
		);
		const getMergeAnalysisSummary = vi
			.fn<DesktopClient["getMergeAnalysisSummary"]>()
			.mockResolvedValueOnce(analysisSummary("running"))
			.mockResolvedValueOnce(analysisSummary("cancelled"));
		const client = createClient({ cancelMergeAnalysis, getMergeAnalysisSummary });
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		fireEvent.click(await screen.findByRole("button", { name: "Cancel analysis" }));

		expect(
			await screen.findByRole("heading", { name: "Cancelled" }),
		).toBeInTheDocument();
		expect(cancelMergeAnalysis).toHaveBeenCalledWith("analysis-1");
		expect(
			screen.getByRole("heading", { name: "No review was produced" }),
		).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Check input again" }));
		expect(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
		expect(client.inspectInput).toHaveBeenCalledTimes(2);
	});

	it("confirms an empty folder twice, then shows the English export result", async (): Promise<void> => {
		const client = createClient({
			getMergeAnalysisSummary: vi.fn(async (): Promise<MergeAnalysisSummary> =>
				analysisSummary("ready"),
			),
			commitMergeAnalysis: vi.fn(
				async (analysisId: string, outputDir: string): Promise<MergeCommitResult> => ({
					analysisId,
					outputDir,
					mergeStatus: "ready",
					generatedFileCount: 3,
					manualConflictCount: 0,
					unsupportedInputCount: 0,
					engineFailureCount: 0,
					committedAtNote: "english note",
				}),
			),
		});
		renderMerge(client, 10_000);
		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		expect(
			await screen.findByRole("heading", { name: "Export Merged Mod" }),
		).toBeInTheDocument();
		expect(
			screen.getByText(/Export never edits Steam, the game directory, or source mods/),
		).toBeInTheDocument();
		const exportButton = screen.getByRole("button", { name: "Export Merged Mod" });
		expect(exportButton).toBeDisabled();
		fireEvent.change(screen.getByRole("textbox", { name: "Output folder" }), {
			target: { value: "  D:\\merged-mod  " },
		});
		fireEvent.click(screen.getByRole("button", { name: "Export Merged Mod" }));
		expect(client.commitMergeAnalysis).not.toHaveBeenCalled();
		fireEvent.click(
			screen.getByRole("button", { name: "Confirm export to this folder" }),
		);
		await waitFor((): void => {
			expect(client.commitMergeAnalysis).toHaveBeenCalledWith(
				"analysis-1",
				"  D:\\merged-mod  ",
			);
		});
		expect(await screen.findByText(/D:\\merged-mod/)).toBeInTheDocument();
		expect(screen.getByRole("textbox", { name: "Output folder" })).toHaveValue(
			"  D:\\merged-mod  ",
		);
		expect(screen.getByText(/Generated files 3/)).toBeInTheDocument();
		expect(
			screen.getByText(/Safe files were written to an empty folder/),
		).toBeInTheDocument();
		expect(screen.queryByText("english note")).not.toBeInTheDocument();
		expect(
			screen.getByRole("button", {
				name: "This analysis has already been exported.",
			}),
		).toBeDisabled();
	});

	it("shows a non-empty target error and keeps the analysis exportable", async (): Promise<void> => {
		const client = createClient({
			commitMergeAnalysis: vi.fn(async (): Promise<MergeCommitResult> => {
				throw {
					code: "output_not_empty",
					message: "choose a new or empty folder; C:\\occupied already contains files",
				};
			}),
		});
		renderMerge(client, 10_000);
		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		fireEvent.change(await screen.findByRole("textbox", { name: "Output folder" }), {
			target: { value: "C:\\occupied" },
		});
		fireEvent.click(screen.getByRole("button", { name: "Export Merged Mod" }));
		fireEvent.click(
			screen.getByRole("button", { name: "Confirm export to this folder" }),
		);
		expect(await screen.findByText(/already contains files/)).toBeInTheDocument();
		expect(screen.getByRole("button", { name: "Export Merged Mod" })).toBeEnabled();
	});

	it("hides export while the analysis is blocked", async (): Promise<void> => {
		const client = createClient({
			getMergeAnalysisSummary: vi.fn(async (): Promise<MergeAnalysisSummary> =>
				analysisSummary("blocked"),
			),
		});
		renderMerge(client, 10_000);
		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		expect(
			await screen.findByRole("heading", { name: "Review blocked" }),
		).toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Export Merged Mod" }),
		).not.toBeInTheDocument();
		expect(client.commitMergeAnalysis).not.toHaveBeenCalled();
	});

	it("exports a deferred analysis in simplified Chinese without the English note", async (): Promise<void> => {
		const client = createClient({
			getDesktopSettings: vi.fn(async (): Promise<DesktopPreferences> => ({
				...ENGLISH_SETTINGS,
				language: "zh_cn",
				effectiveLanguage: "zh-CN",
			})),
		});
		render(<App client={client} pollIntervalMs={10_000} />);
		fireEvent.click(await screen.findByRole("tab", { name: "合并与冲突" }));
		fireEvent.click(await screen.findByRole("button", { name: /分析当前剧本组/ }));
		expect(
			await screen.findByRole("heading", { name: "导出合并 Mod" }),
		).toBeInTheDocument();
		expect(
			screen.getByText(/导出不会修改 Steam、游戏目录或源 Mod/),
		).toBeInTheDocument();
		fireEvent.change(screen.getByRole("textbox", { name: "输出文件夹" }), {
			target: { value: "D:\\导出" },
		});
		fireEvent.click(screen.getByRole("button", { name: "导出合并 Mod" }));
		fireEvent.click(screen.getByRole("button", { name: "确认导出到此文件夹" }));
		await waitFor((): void => {
			expect(client.commitMergeAnalysis).toHaveBeenCalledWith("analysis-1", "D:\\导出");
		});
		expect(screen.getByText("D:\\导出")).toBeInTheDocument();
		expect(screen.getByText(/生成的文件 4/)).toBeInTheDocument();
		expect(
			screen.getByText(/已写入安全的文件和定义模块。不安全的单元已延后/),
		).toBeInTheDocument();
		expect(screen.queryByText("english note")).not.toBeInTheDocument();
		expect(screen.getByRole("button", { name: "这次分析已经导出。" })).toBeDisabled();
	});

	it("renders leaf conflicts with vanilla and competing snippets in English", async (): Promise<void> => {
		const getMergeUnit = vi.fn<DesktopClient["getMergeUnit"]>(
			async (): Promise<MergeUnitDetail> => CONFLICT_UNIT_DETAIL,
		);
		const client = createClient({ getMergeUnit });
		renderMerge(client, 10_000);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		expect(
			await screen.findByRole("heading", { name: "Review required" }),
		).toBeInTheDocument();
		await screen.findByText("6 results");
		fireEvent.click(
			screen.getByRole("button", { name: /missions\/french_missions\.txt/i }),
		);
		expect(await screen.findByText("Leaf conflicts")).toBeInTheDocument();
		expect(screen.getByText("manual conflict")).toBeInTheDocument();
		expect(screen.getByText("Vanilla")).toBeInTheDocument();
		expect(screen.getByText("Competing mods")).toBeInTheDocument();
		expect(screen.getByText("religion = orthodox")).toBeInTheDocument();
		expect(screen.getByText("religion = protestant")).toBeInTheDocument();
		// The vanilla snippet and the baseline candidate both render it.
		expect(screen.getAllByText("religion = catholic").length).toBe(2);
		// The contributor list and the candidate card both name Conflict B.
		expect(screen.getAllByText("Conflict B").length).toBe(2);
		// Baseline and Conflict A carry no explicit option, so their buttons
		// render disabled; only Conflict B's choices are enabled.
		const fileButtons = screen.getAllByRole("button", {
			name: "Prefer for this file",
		});
		expect(fileButtons).toHaveLength(3);
		expect(fileButtons[0]).toBeDisabled();
		expect(fileButtons[1]).toBeDisabled();
		expect(fileButtons[2]).toBeEnabled();
		const folderButtons = screen.getAllByRole("button", {
			name: "Prefer for folder",
		});
		expect(folderButtons).toHaveLength(3);
		expect(folderButtons[0]).toBeDisabled();
		expect(folderButtons[1]).toBeDisabled();
		expect(folderButtons[2]).toBeEnabled();
	});

	it("queues a file-scope resolution, re-analyzes with it, and previews the foch.toml snippet", async (): Promise<void> => {
		const getMergeUnit = vi.fn<DesktopClient["getMergeUnit"]>(
			async (): Promise<MergeUnitDetail> => CONFLICT_UNIT_DETAIL,
		);
		const startMergeAnalysis = vi.fn<DesktopClient["startMergeAnalysis"]>(
			async (): Promise<StartMergeAnalysisResult> => ({
				analysisId: "analysis-1",
				inputScope: COMPLETE_INPUT_SCOPE,
			}),
		);
		const renderResolutionsToml = vi.fn<DesktopClient["renderResolutionsToml"]>(
			async (): Promise<string> =>
				'[[resolutions]]\nfile = "history/countries/TES - Test.txt"\nprefer_mod = "ugc-300002"\n',
		);
		const checked = setupFromInspection(READY_INSPECTION);
		const rechecked = setupFromInspection({
			...READY_INSPECTION,
			inspectionId: "inspection-recheck",
		});
		let setupStatus: SetupStatus = checked;
		let recheckPolls = 0;
		const startSetupCheck = vi.fn(async (): Promise<SetupStatus> => checked);
		startSetupCheck.mockImplementation(async (): Promise<SetupStatus> => {
			if (startSetupCheck.mock.calls.length === 1) {
				setupStatus = checked;
				return checked;
			}
			// The real command returns before the inspection finishes.
			setupStatus = {
				...checked,
				phase: "checking",
				stage: "game_install",
			};
			return setupStatus;
		});
		const getSetupStatus = vi.fn(async (): Promise<SetupStatus> => {
			if (setupStatus.phase !== "checking") return setupStatus;
			recheckPolls += 1;
			if (recheckPolls < 2) return setupStatus;
			setupStatus = rechecked;
			return rechecked;
		});
		const client = createClient({
			getMergeUnit,
			startMergeAnalysis,
			renderResolutionsToml,
			startSetupCheck,
			getSetupStatus,
		});
		renderMerge(client, 20);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		expect(
			await screen.findByRole("heading", { name: "Review required" }),
		).toBeInTheDocument();
		await screen.findByText("6 results");
		fireEvent.click(
			screen.getByRole("button", { name: /missions\/french_missions\.txt/i }),
		);
		await screen.findByText("Leaf conflicts");
		// Conflict B is the third candidate and the only one with options.
		const enabledFileChoice = screen.getAllByRole("button", {
			name: "Prefer for this file",
		})[2];
		if (enabledFileChoice === undefined)
			throw new Error("no enabled file-scope prefer button");
		fireEvent.click(enabledFileChoice);

		expect(
			await screen.findByRole("heading", { name: /Pending resolutions \(1\)/ }),
		).toBeInTheDocument();
		expect(
			screen.getByText(/file · history\/countries\/TES - Test\.txt/),
		).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "Copy foch.toml snippet" }));
		const tomlMatches = await screen.findAllByText(/\[\[resolutions\]\]/);
		expect(tomlMatches.length).toBeGreaterThanOrEqual(1);
		expect(screen.getByText(/prefer_mod = "ugc-300002"/)).toBeInTheDocument();
		expect(renderResolutionsToml).toHaveBeenCalledWith([PREFER_B_FILE_RULE]);

		fireEvent.click(
			screen.getByRole("button", { name: "Re-analyze with resolutions" }),
		);
		await waitFor((): void => {
			expect(startMergeAnalysis).toHaveBeenLastCalledWith(
				"inspection-recheck",
				"complete",
				[PREFER_B_FILE_RULE],
			);
		});
		expect(recheckPolls).toBeGreaterThanOrEqual(2);
		expect(getSetupStatus).toHaveBeenCalled();
		await waitFor((): void => {
			expect(
				screen.queryByRole("heading", { name: /Pending resolutions/ }),
			).not.toBeInTheDocument();
		});
	});

	it("renders leaf conflicts in simplified Chinese and queues a folder rule", async (): Promise<void> => {
		const client = createClient({
			getDesktopSettings: vi.fn(async (): Promise<DesktopPreferences> => ({
				...ENGLISH_SETTINGS,
				language: "zh_cn",
				effectiveLanguage: "zh-CN",
			})),
			getMergeUnit: vi.fn(async (): Promise<MergeUnitDetail> => CONFLICT_UNIT_DETAIL),
		});
		render(<App client={client} pollIntervalMs={10_000} />);
		fireEvent.click(await screen.findByRole("tab", { name: "合并与冲突" }));
		fireEvent.click(await screen.findByRole("button", { name: /分析当前剧本组/ }));
		expect(
			await screen.findByRole("heading", { name: "需要审查" }),
		).toBeInTheDocument();
		await screen.findByText("6 条结果");
		fireEvent.click(
			screen.getByRole("button", { name: /missions\/french_missions\.txt/i }),
		);
		expect(await screen.findByText("叶子冲突")).toBeInTheDocument();
		expect(screen.getByText("原版")).toBeInTheDocument();
		expect(screen.getByText("竞争 Mod")).toBeInTheDocument();
		expect(screen.getByText("religion = protestant")).toBeInTheDocument();
		const enabledFolderChoice = screen.getAllByRole("button", {
			name: "同目录优先此 Mod",
		})[2];
		if (enabledFolderChoice === undefined)
			throw new Error("no enabled folder-scope prefer button");
		fireEvent.click(enabledFolderChoice);
		expect(
			await screen.findByRole("heading", { name: /待应用解决规则 \(1\)/ }),
		).toBeInTheDocument();
		expect(screen.getByText(/目录 · re:/)).toBeInTheDocument();
		fireEvent.click(screen.getByRole("button", { name: "复制 foch.toml 配置" }));
		const tomlMatches = await screen.findAllByText(/\[\[resolutions\]\]/);
		expect(tomlMatches.length).toBeGreaterThanOrEqual(1);
		expect(
			screen.getByText(/把这段 \[\[resolutions\]\] 加入你自己的 foch\.toml/),
		).toBeInTheDocument();
	});
});

describe("Foch desktop play, plugins, and settings", (): void => {
	it("lands on Play & Launch and shows a companion alert", async (): Promise<void> => {
		const client = createClient({
			getPluginOverview: vi.fn(
				async (): Promise<DesktopPluginOverview> => PLUGIN_OVERVIEW,
			),
		});
		render(<App client={client} />);

		expect(screen.getByRole("heading", { name: "Play & Launch" })).toBeInTheDocument();
		expect(
			screen.queryByRole("heading", { name: "Checking EU4 setup" }),
		).not.toBeInTheDocument();
		expect(
			await screen.findByText(
				"Europa Expanded declares dev.foch.sample, which is not installed",
			),
		).toBeInTheDocument();
		expect(screen.getByText("dev.foch.slider 0.1.0")).toBeInTheDocument();

		fireEvent.click(screen.getByRole("button", { name: "Install Sample" }));
		await waitFor((): void => {
			expect(client.installPlugin).toHaveBeenCalledWith("Acture/sample", null);
		});
	});

	it("toggles a plugin and writes a schema slider value", async (): Promise<void> => {
		const client = createClient({
			getPluginOverview: vi.fn(
				async (): Promise<DesktopPluginOverview> => PLUGIN_OVERVIEW,
			),
		});
		render(<App client={client} />);
		fireEvent.click(screen.getByRole("tab", { name: "Plugins" }));

		fireEvent.click(await screen.findByRole("switch", { name: "Toggle Slider" }));
		await waitFor((): void => {
			expect(client.setPluginEnabled).toHaveBeenCalledWith(
				"Campaign 1444",
				"dev.foch.slider",
				false,
				"0.1.0",
			);
		});

		fireEvent.change(screen.getByRole("slider", { name: "limit" }), {
			target: { value: "4" },
		});
		await waitFor((): void => {
			expect(client.updatePluginConfig).toHaveBeenCalledWith(
				"Campaign 1444",
				"dev.foch.slider",
				"limit",
				4,
			);
		});
	});

	it("prepares a dry run without starting EU4", async (): Promise<void> => {
		const client = createClient({
			getPluginOverview: vi.fn(
				async (): Promise<DesktopPluginOverview> => PLUGIN_OVERVIEW,
			),
		});
		render(<App client={client} />);

		fireEvent.click(
			await screen.findByRole("button", { name: "Prepare Only (Dry-Run)" }),
		);
		await waitFor((): void => {
			expect(client.launchEu4WithPlugins).toHaveBeenCalledWith("Campaign 1444", true);
		});
		expect(await screen.findByText("Active")).toBeInTheDocument();
		expect(screen.getByText("Prepared only")).toBeInTheDocument();
		expect(client.launchEu4WithPlugins).not.toHaveBeenCalledWith(
			"Campaign 1444",
			false,
		);
	});

	it("saves transport, mirror, and switches the interface to simplified Chinese", async (): Promise<void> => {
		const client = createClient();
		render(<App client={client} />);
		fireEvent.click(screen.getByRole("tab", { name: "Settings" }));

		fireEvent.change(await screen.findByRole("combobox", { name: "Transport" }), {
			target: { value: "http" },
		});
		fireEvent.change(await screen.findByRole("combobox", { name: "Mirror" }), {
			target: { value: "cn" },
		});
		fireEvent.change(screen.getByRole("combobox", { name: "Language" }), {
			target: { value: "zh_cn" },
		});

		expect(await screen.findByRole("tab", { name: "插件" })).toBeInTheDocument();
		expect(screen.getByRole("heading", { name: "设置" })).toBeInTheDocument();
		await waitFor((): void => {
			expect(client.saveDesktopSettings).toHaveBeenLastCalledWith(
				expect.objectContaining({
					transport: "http",
					mirror: "cn",
					language: "zh_cn",
				}),
			);
		});
	});

	it("tests the unsaved settings draft and shows each route in English", async (): Promise<void> => {
		const client = createClient();
		render(<App client={client} />);
		fireEvent.click(screen.getByRole("tab", { name: "Settings" }));
		fireEvent.change(await screen.findByRole("combobox", { name: "Mirror" }), {
			target: { value: "custom" },
		});
		fireEvent.change(screen.getByRole("textbox", { name: "Custom HTTPS prefixes" }), {
			target: { value: "https://mirror.example/" },
		});
		fireEvent.click(screen.getByRole("button", { name: "Test Connectivity" }));
		await waitFor((): void => {
			expect(client.testDownloadConnectivity).toHaveBeenCalledWith({
				transport: "auto",
				mirror: "custom",
				customMirrors: ["https://mirror.example/"],
				language: "auto",
			});
		});
		expect(client.saveDesktopSettings).not.toHaveBeenCalled();
		expect(await screen.findByText("Reachable")).toBeInTheDocument();
		expect(screen.getByText("HTTP 200")).toBeInTheDocument();
		expect(screen.getByText(/Latency 12 ms/)).toBeInTheDocument();
		expect(screen.getByText(/SHA-256 and PE export validation/)).toBeInTheDocument();
	});

	it("shows an unreachable route in simplified Chinese", async (): Promise<void> => {
		const client = createClient({
			getDesktopSettings: vi.fn(async (): Promise<DesktopPreferences> => ({
				...ENGLISH_SETTINGS,
				language: "zh_cn",
				effectiveLanguage: "zh-CN",
			})),
			testDownloadConnectivity: vi.fn(async (): Promise<DesktopConnectivityReport> => ({
				transport: "http",
				mirrorPolicy: "direct",
				routes: [
					{
						label: "direct",
						url: "https://github.com/Acture/foch",
						kind: "direct",
						reachable: false,
						httpStatus: 502,
						latencyMs: 40,
						detail: "HEAD 502",
					},
				],
			})),
		});
		render(<App client={client} />);
		fireEvent.click(await screen.findByRole("tab", { name: "设置" }));
		fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
		expect(await screen.findByText("不可达")).toBeInTheDocument();
		expect(screen.getByText("HTTP 502")).toBeInTheDocument();
		expect(screen.getByText(/延迟 40 ms/)).toBeInTheDocument();
		expect(screen.getByText(/SHA-256/)).toBeInTheDocument();
		expect(client.saveDesktopSettings).not.toHaveBeenCalled();
	});
});
