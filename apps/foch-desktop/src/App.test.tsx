import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import App from "./App";
import type {
	DesktopClient,
	InputInspection,
	MergeAnalysisState,
	MergeAnalysisSummary,
	MergeUnitDetail,
	MergeUnitPage,
} from "./api";

const READY_INSPECTION: InputInspection = {
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
				sourceError: "descriptor.mod could not be read",
			},
		],
	},
	issues: [],
};

const BLOCKED_INSPECTION: InputInspection = {
	...READY_INSPECTION,
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
	contributors: [
		{ modId: "base", name: "Europa Universalis IV", position: 0, isBaseGame: true },
		{ modId: "mod-1", name: "Europa Expanded", position: 1, isBaseGame: false },
	],
	notes: ["Base snapshot used as the semantic ancestor."],
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

function createClient(overrides: Partial<DesktopClient> = {}): DesktopClient {
	return {
		inspectInput: vi.fn(async (): Promise<InputInspection> => READY_INSPECTION),
		startMergeAnalysis: vi.fn(async () => ({ analysisId: "analysis-1" })),
		cancelMergeAnalysis: vi.fn(async (): Promise<void> => undefined),
		getMergeAnalysisSummary: vi.fn(async (): Promise<MergeAnalysisSummary> =>
			analysisSummary("ready_with_deferrals"),
		),
		listMergeUnits: vi.fn(async (): Promise<MergeUnitPage> => UNIT_PAGE),
		getMergeUnit: vi.fn(async (): Promise<MergeUnitDetail> => UNIT_DETAIL),
		...overrides,
	};
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
	it("shows first-launch loading, then the detected playset and source identity", async (): Promise<void> => {
		const pendingInspection = deferred<InputInspection>();
		const client = createClient({
			inspectInput: vi.fn((): Promise<InputInspection> => pendingInspection.promise),
		});

		render(<App client={client} />);
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
			screen.getByText("Source error: descriptor.mod could not be read"),
		).toBeInTheDocument();
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

		render(<App client={client} />);

		expect(
			await screen.findByRole("heading", {
				name: "Resolve setup blockers before analysis",
			}),
		).toBeInTheDocument();
		expect(screen.getByText("EU4 base data is missing")).toBeInTheDocument();
		expect(
			screen.queryByRole("button", { name: /Analyze current playset/i }),
		).not.toBeInTheDocument();

		fireEvent.click(screen.getByRole("button", { name: "Check again" }));
		expect(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		).toBeEnabled();
		expect(inspectInput).toHaveBeenCalledTimes(2);
	});

	it("runs read-only analysis and exposes every merge-unit outcome with focused detail", async (): Promise<void> => {
		const client = createClient();
		render(<App client={client} pollIntervalMs={10_000} />);

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
		expect(
			await screen.findByText("Both contributions can be preserved without a choice."),
		).toBeInTheDocument();
		expect(
			screen.getByText("Base snapshot used as the semantic ancestor."),
		).toBeInTheDocument();
	});

	it("passes search and outcome filters through the bounded list endpoint", async (): Promise<void> => {
		const listMergeUnits = vi.fn<DesktopClient["listMergeUnits"]>(
			async (): Promise<MergeUnitPage> => UNIT_PAGE,
		);
		const client = createClient({ listMergeUnits });
		render(<App client={client} pollIntervalMs={10_000} />);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		await screen.findByText("6 results");

		fireEvent.change(screen.getByPlaceholderText("Search path or family"), {
			target: { value: "missions" },
		});
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

	it("cancels an active analysis by opaque analysis id", async (): Promise<void> => {
		const cancelMergeAnalysis = vi.fn<DesktopClient["cancelMergeAnalysis"]>(
			async (): Promise<void> => undefined,
		);
		const getMergeAnalysisSummary = vi
			.fn<DesktopClient["getMergeAnalysisSummary"]>()
			.mockResolvedValueOnce(analysisSummary("running"))
			.mockResolvedValueOnce(analysisSummary("cancelled"));
		const client = createClient({ cancelMergeAnalysis, getMergeAnalysisSummary });
		render(<App client={client} pollIntervalMs={10_000} />);

		fireEvent.click(
			await screen.findByRole("button", { name: /Analyze current playset/i }),
		);
		fireEvent.click(await screen.findByRole("button", { name: "Cancel analysis" }));

		expect(
			await screen.findByRole("heading", { name: "Cancelled" }),
		).toBeInTheDocument();
		expect(cancelMergeAnalysis).toHaveBeenCalledWith("analysis-1");
	});
});
