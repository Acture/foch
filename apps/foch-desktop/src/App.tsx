import "./App.css";

import {
	type CSSProperties,
	type FormEvent,
	type JSX,
	useCallback,
	useEffect,
	useMemo,
	useState,
} from "react";

import {
	MERGE_DISPOSITIONS,
	type DesktopClient,
	type InputInspection,
	type MergeAnalysisStage,
	type MergeAnalysisState,
	type MergeAnalysisSummary,
	type MergeDisposition,
	type MergeUnitDetail,
	type MergeUnitListItem,
	type MergeUnitPage,
	isAnalysisActive,
	tauriDesktopClient,
} from "./api";

const PAGE_SIZE = 12;

const DISPOSITION_LABELS: Record<MergeDisposition, string> = {
	safe: "Safe merge",
	copy: "Copy through",
	needs_user_choice: "Needs a choice",
	unsupported_input: "Unsupported input",
	engine_failure: "Engine failure",
	deferred: "Deferred",
};

const STAGE_LABELS: Record<MergeAnalysisStage, string> = {
	inventory: "Inventory",
	resolve_input: "Resolve input",
	semantic_merge: "Analyze merge units",
	validate_output: "Validate output",
	freeze_artifacts: "Freeze analyzed bytes",
	complete: "Complete",
};

const ANALYSIS_STATE_LABELS: Record<MergeAnalysisState, string> = {
	queued: "Queued",
	running: "Analyzing",
	ready: "Ready",
	ready_with_deferrals: "Review required",
	blocked: "Blocked",
	cancelled: "Cancelled",
	failed: "Failed",
};

interface AppProps {
	client?: DesktopClient;
	pollIntervalMs?: number;
}

interface AsyncValue<T> {
	value: T | null;
	loading: boolean;
	error: string | null;
}

const EMPTY_UNIT_PAGE: MergeUnitPage = {
	items: [],
	total: 0,
	page: 1,
	pageSize: PAGE_SIZE,
};

function initialAsyncValue<T>(): AsyncValue<T> {
	return { value: null, loading: true, error: null };
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function formatElapsed(elapsedMs: number): string {
	const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1_000));
	const minutes = Math.floor(totalSeconds / 60);
	const seconds = totalSeconds % 60;
	return minutes > 0
		? `${minutes}m ${seconds.toString().padStart(2, "0")}s`
		: `${seconds}s`;
}

function dispositionClass(disposition: MergeDisposition): string {
	return `disposition-${disposition.replace(/_/g, "-")}`;
}

function StatusMark({ disposition }: { disposition: MergeDisposition }): JSX.Element {
	return (
		<span className={`status-mark ${dispositionClass(disposition)}`}>
			<span className="status-mark-dot" aria-hidden="true" />
			{DISPOSITION_LABELS[disposition]}
		</span>
	);
}

function ReadinessRail({ inspection }: { inspection: InputInspection }): JSX.Element {
	const enabledMods =
		inspection.playset?.mods.filter((mod): boolean => mod.enabled) ?? [];

	return (
		<aside className="input-rail" aria-label="Detected EU4 input">
			<div className="rail-section rail-readiness">
				<p className="section-kicker">Input readiness</p>
				<div className={`readiness-seal readiness-${inspection.readiness}`}>
					<span aria-hidden="true">{inspection.readiness === "ready" ? "✓" : "!"}</span>
					<strong>
						{inspection.readiness === "ready" ? "Ready to analyze" : "Setup blocked"}
					</strong>
				</div>
			</div>

			<div className="rail-section facts-list">
				<div>
					<span>Game</span>
					<strong>{inspection.game.name}</strong>
				</div>
				<div>
					<span>Version</span>
					<strong>{inspection.game.version ?? "Not detected"}</strong>
				</div>
				<div>
					<span>EU4 base</span>
					<strong title={inspection.baseData.detail}>
						{inspection.baseData.state.replace(/_/g, " ")}
						{inspection.baseData.version !== null &&
							` · ${inspection.baseData.version}`}
					</strong>
				</div>
			</div>

			<div className="rail-section load-order-section">
				<div className="section-heading-row">
					<div>
						<p className="section-kicker">Current playset</p>
						<h2>{inspection.playset?.name ?? "Not detected"}</h2>
					</div>
					<span className="compact-count">{enabledMods.length}</span>
				</div>

				{inspection.playset === null ? (
					<p className="rail-empty">No current EU4 playset was found.</p>
				) : (
					<ol className="load-order" aria-label="Detected mod load order">
						{inspection.playset.mods.map((mod): JSX.Element => (
							<li key={mod.id} className={mod.enabled ? "" : "mod-disabled"}>
								<span className="load-position">
									{mod.position.toString().padStart(2, "0")}
								</span>
								<span className="mod-copy">
									<span className="mod-name">{mod.name}</span>
									<span className="mod-identity">
										{mod.workshopId === null
											? "Local mod"
											: `Workshop ${mod.workshopId}`}
										{mod.version !== null && ` · v${mod.version}`}
										{mod.workshopManifestId !== null &&
											` · manifest ${mod.workshopManifestId}`}
									</span>
									{mod.declaredDependencies.length > 0 && (
										<span className="mod-dependencies">
											Depends on: {mod.declaredDependencies.join(", ")}
										</span>
									)}
									{mod.sourceError !== null && (
										<span
											className="mod-source-error"
											title={mod.descriptorPath ?? undefined}
										>
											Source error: {mod.sourceError}
										</span>
									)}
								</span>
								{!mod.enabled && <span className="mod-state">Off</span>}
							</li>
						))}
					</ol>
				)}
			</div>
		</aside>
	);
}

function DispositionRibbon({
	summary,
}: {
	summary: MergeAnalysisSummary;
}): JSX.Element {
	const counts: Record<MergeDisposition, number> = {
		safe: summary.counts.safe,
		copy: summary.counts.copy,
		needs_user_choice: summary.counts.needsUserChoice,
		unsupported_input: summary.counts.unsupportedInput,
		engine_failure: summary.counts.engineFailure,
		deferred: summary.counts.deferred,
	};

	return (
		<div className="outcome-overview" aria-label="Merge unit outcomes">
			<div className="outcome-ribbon" aria-hidden="true">
				{MERGE_DISPOSITIONS.map((disposition): JSX.Element => {
					const style = {
						"--outcome-weight": Math.max(counts[disposition], 0.2),
					} as CSSProperties;
					return (
						<span
							key={disposition}
							className={`outcome-segment ${dispositionClass(disposition)}`}
							style={style}
						/>
					);
				})}
			</div>
			<div className="outcome-legend">
				{MERGE_DISPOSITIONS.map((disposition): JSX.Element => (
					<div key={disposition} className="outcome-stat">
						<span
							className={`legend-swatch ${dispositionClass(disposition)}`}
							aria-hidden="true"
						/>
						<span>{DISPOSITION_LABELS[disposition]}</span>
						<strong>{counts[disposition]}</strong>
					</div>
				))}
			</div>
		</div>
	);
}

function AnalysisHeader({
	summary,
	cancelling,
	onCancel,
}: {
	summary: MergeAnalysisSummary;
	cancelling: boolean;
	onCancel: () => void;
}): JSX.Element {
	const progress =
		summary.totalUnits === 0
			? 0
			: Math.min(100, Math.round((summary.completedUnits / summary.totalUnits) * 100));
	const active = isAnalysisActive(summary.state);

	return (
		<section className="analysis-header" aria-live="polite">
			<div className="analysis-title-row">
				<div>
					<p className="section-kicker">Read-only analysis</p>
					<h2>{ANALYSIS_STATE_LABELS[summary.state]}</h2>
				</div>
				{active && (
					<button
						className="secondary-action danger-action"
						onClick={onCancel}
						disabled={cancelling}
					>
						{cancelling ? "Cancelling…" : "Cancel analysis"}
					</button>
				)}
			</div>
			<div className="progress-copy">
				<strong>{STAGE_LABELS[summary.stage]}</strong>
				<span>
					{summary.completedUnits.toLocaleString()} /{" "}
					{summary.totalUnits.toLocaleString()} units
				</span>
				<span>{formatElapsed(summary.elapsedMs)} elapsed</span>
			</div>
			<div
				className="progress-track"
				role="progressbar"
				aria-label="Merge analysis progress"
				aria-valuemin={0}
				aria-valuemax={100}
				aria-valuenow={progress}
			>
				<span style={{ width: `${progress}%` }} />
			</div>
			{summary.message !== null && (
				<p className="analysis-message">{summary.message}</p>
			)}
		</section>
	);
}

function UnitRow({
	unit,
	selected,
	onSelect,
}: {
	unit: MergeUnitListItem;
	selected: boolean;
	onSelect: (id: string) => void;
}): JSX.Element {
	return (
		<button
			className={`unit-row ${selected ? "unit-row-selected" : ""}`}
			type="button"
			onClick={(): void => onSelect(unit.id)}
			aria-pressed={selected}
		>
			<span className="unit-path">{unit.path}</span>
			<span className="unit-meta">
				{unit.family} ·{" "}
				{unit.kind === "definition_module" ? "Definition module" : "File"} ·{" "}
				{unit.contributorCount} sources
			</span>
			<StatusMark disposition={unit.disposition} />
		</button>
	);
}

function UnitDetailPanel({
	detail,
}: {
	detail: AsyncValue<MergeUnitDetail>;
}): JSX.Element {
	if (detail.loading) {
		return (
			<aside className="detail-pane" aria-label="Merge unit detail">
				<p className="panel-placeholder">Loading unit detail…</p>
			</aside>
		);
	}
	if (detail.error !== null) {
		return (
			<aside className="detail-pane" aria-label="Merge unit detail">
				<p className="error-copy">Unit detail unavailable: {detail.error}</p>
			</aside>
		);
	}
	if (detail.value === null) {
		return (
			<aside className="detail-pane detail-empty" aria-label="Merge unit detail">
				<p className="section-kicker">Unit detail</p>
				<h2>Select a merge unit</h2>
				<p>Choose a row to inspect its outcome and ordered contributors.</p>
			</aside>
		);
	}

	const unit = detail.value;
	return (
		<aside className="detail-pane" aria-label="Merge unit detail">
			<div className="detail-heading">
				<p className="section-kicker">Selected merge unit</p>
				<h2>{unit.path}</h2>
				<StatusMark disposition={unit.disposition} />
			</div>
			<dl className="detail-facts">
				<div>
					<dt>Family</dt>
					<dd>{unit.family}</dd>
				</div>
				<div>
					<dt>Strategy</dt>
					<dd>{unit.strategy}</dd>
				</div>
				<div>
					<dt>Unit</dt>
					<dd>{unit.kind === "definition_module" ? "Definition module" : "File"}</dd>
				</div>
				{unit.outputPath !== null && (
					<div>
						<dt>Output</dt>
						<dd className="path-value">{unit.outputPath}</dd>
					</div>
				)}
			</dl>
			<section className="detail-section">
				<h3>Outcome</h3>
				<p>{unit.summary}</p>
			</section>
			<section className="detail-section">
				<h3>Load-order contributors</h3>
				<ol className="contributor-list">
					{unit.contributors.map((contributor): JSX.Element => (
						<li key={`${contributor.modId}-${contributor.position}`}>
							<span>{contributor.position.toString().padStart(2, "0")}</span>
							<strong>{contributor.name}</strong>
							{contributor.isBaseGame && <small>Base game</small>}
						</li>
					))}
				</ol>
			</section>
			{unit.notes.length > 0 && (
				<section className="detail-section">
					<h3>Review notes</h3>
					<ul className="note-list">
						{unit.notes.map((note): JSX.Element => (
							<li key={note}>{note}</li>
						))}
					</ul>
				</section>
			)}
		</aside>
	);
}

export default function App({
	client = tauriDesktopClient,
	pollIntervalMs = 900,
}: AppProps): JSX.Element {
	const [inspection, setInspection] =
		useState<AsyncValue<InputInspection>>(initialAsyncValue<InputInspection>());
	const [starting, setStarting] = useState<boolean>(false);
	const [cancelling, setCancelling] = useState<boolean>(false);
	const [analysisId, setAnalysisId] = useState<string | null>(null);
	const [summary, setSummary] = useState<MergeAnalysisSummary | null>(null);
	const [analysisError, setAnalysisError] = useState<string | null>(null);
	const [unitPage, setUnitPage] = useState<AsyncValue<MergeUnitPage>>({
		value: EMPTY_UNIT_PAGE,
		loading: false,
		error: null,
	});
	const [selectedUnitId, setSelectedUnitId] = useState<string | null>(null);
	const [unitDetail, setUnitDetail] = useState<AsyncValue<MergeUnitDetail>>({
		value: null,
		loading: false,
		error: null,
	});
	const [searchDraft, setSearchDraft] = useState<string>("");
	const [searchQuery, setSearchQuery] = useState<string>("");
	const [disposition, setDisposition] = useState<MergeDisposition | null>(null);
	const [page, setPage] = useState<number>(1);

	const inspectInput = useCallback(async (): Promise<void> => {
		setInspection((current): AsyncValue<InputInspection> => ({
			value: current.value,
			loading: true,
			error: null,
		}));
		try {
			setInspection({
				value: await client.inspectInput(),
				loading: false,
				error: null,
			});
		} catch (error: unknown) {
			setInspection({ value: null, loading: false, error: errorMessage(error) });
		}
	}, [client]);

	useEffect((): void => {
		void inspectInput();
	}, [inspectInput]);

	useEffect((): (() => void) | undefined => {
		if (analysisId === null) return undefined;
		let disposed = false;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const refresh = async (): Promise<void> => {
			try {
				const next = await client.getMergeAnalysisSummary(analysisId);
				if (disposed) return;
				setSummary(next);
				setAnalysisError(null);
				if (isAnalysisActive(next.state)) {
					timer = setTimeout((): void => {
						void refresh();
					}, pollIntervalMs);
				}
			} catch (error: unknown) {
				if (!disposed) setAnalysisError(errorMessage(error));
			}
		};
		void refresh();
		return (): void => {
			disposed = true;
			if (timer !== undefined) clearTimeout(timer);
		};
	}, [analysisId, client, pollIntervalMs]);

	useEffect((): (() => void) | undefined => {
		if (analysisId === null || summary === null || summary.state === "queued")
			return undefined;
		let disposed = false;
		setUnitPage((current): AsyncValue<MergeUnitPage> => ({
			value: current.value,
			loading: true,
			error: null,
		}));
		void client
			.listMergeUnits({
				analysisId,
				query: searchQuery,
				disposition,
				page,
				pageSize: PAGE_SIZE,
			})
			.then((value): void => {
				if (disposed) return;
				setUnitPage({ value, loading: false, error: null });
				setSelectedUnitId((current): string | null => {
					if (
						current !== null &&
						value.items.some((item): boolean => item.id === current)
					)
						return current;
					return value.items[0]?.id ?? null;
				});
			})
			.catch((error: unknown): void => {
				if (!disposed)
					setUnitPage({
						value: EMPTY_UNIT_PAGE,
						loading: false,
						error: errorMessage(error),
					});
			});
		return (): void => {
			disposed = true;
		};
	}, [analysisId, client, disposition, page, searchQuery, summary]);

	useEffect((): (() => void) | undefined => {
		if (analysisId === null || selectedUnitId === null) {
			setUnitDetail({ value: null, loading: false, error: null });
			return undefined;
		}
		let disposed = false;
		setUnitDetail({ value: null, loading: true, error: null });
		void client
			.getMergeUnit(analysisId, selectedUnitId)
			.then((value): void => {
				if (!disposed) setUnitDetail({ value, loading: false, error: null });
			})
			.catch((error: unknown): void => {
				if (!disposed)
					setUnitDetail({ value: null, loading: false, error: errorMessage(error) });
			});
		return (): void => {
			disposed = true;
		};
	}, [analysisId, client, selectedUnitId]);

	const startAnalysis = async (): Promise<void> => {
		setStarting(true);
		setAnalysisError(null);
		setSummary(null);
		setSelectedUnitId(null);
		setUnitDetail({ value: null, loading: false, error: null });
		setUnitPage({ value: EMPTY_UNIT_PAGE, loading: false, error: null });
		try {
			setAnalysisId((await client.startMergeAnalysis()).analysisId);
		} catch (error: unknown) {
			setAnalysisError(errorMessage(error));
		} finally {
			setStarting(false);
		}
	};

	const cancelAnalysis = async (): Promise<void> => {
		if (analysisId === null) return;
		setCancelling(true);
		try {
			await client.cancelMergeAnalysis(analysisId);
			setSummary(await client.getMergeAnalysisSummary(analysisId));
			setAnalysisError(null);
		} catch (error: unknown) {
			setAnalysisError(errorMessage(error));
		} finally {
			setCancelling(false);
		}
	};

	const submitSearch = (event: FormEvent<HTMLFormElement>): void => {
		event.preventDefault();
		setPage(1);
		setSearchQuery(searchDraft.trim());
	};

	const pageValue = unitPage.value ?? EMPTY_UNIT_PAGE;
	const pageCount = Math.max(1, Math.ceil(pageValue.total / PAGE_SIZE));
	const playsetName = inspection.value?.playset?.name ?? "Current EU4 playset";
	const canAnalyze = inspection.value?.readiness === "ready" && !starting;
	const showBrowser = analysisId !== null && summary !== null;
	const dispositionOptions = useMemo(
		(): Array<{ value: MergeDisposition; label: string }> =>
			MERGE_DISPOSITIONS.map((value): { value: MergeDisposition; label: string } => ({
				value,
				label: DISPOSITION_LABELS[value],
			})),
		[],
	);

	return (
		<main className="app-shell">
			<header className="app-header">
				<div className="brand-lockup">
					<span className="brand-monogram" aria-hidden="true">
						F
					</span>
					<div>
						<h1>Foch</h1>
						<p>EU4 merge review</p>
					</div>
				</div>
				<div className="header-context">
					<span>Detected input</span>
					<strong>{playsetName}</strong>
				</div>
				<span className="read-only-badge">Read-only checkpoint</span>
			</header>

			{inspection.loading && inspection.value === null ? (
				<section className="launch-state" aria-live="polite">
					<span className="scan-line" aria-hidden="true" />
					<p className="section-kicker">First-launch inspection</p>
					<h2>Checking EU4 setup</h2>
					<p>Detecting the game, base data, and current launcher playset.</p>
				</section>
			) : inspection.error !== null ? (
				<section className="launch-state launch-error" role="alert">
					<p className="section-kicker">Input inspection failed</p>
					<h2>EU4 setup could not be inspected</h2>
					<p>{inspection.error}</p>
					<button className="primary-action" onClick={(): void => void inspectInput()}>
						Check again
					</button>
				</section>
			) : inspection.value === null ? null : (
				<div className="workbench">
					<ReadinessRail inspection={inspection.value} />
					<section className="analysis-workspace" aria-label="Merge analysis workspace">
						{inspection.value.readiness === "blocked" ? (
							<div className="blocked-state">
								<p className="section-kicker">Action required</p>
								<h2>Resolve setup blockers before analysis</h2>
								<p className="state-intro">
									Foch will not infer missing game or base-data inputs.
								</p>
								<ul className="issue-list">
									{inspection.value.issues.map((issue): JSX.Element => (
										<li key={issue.id}>
											<strong>{issue.title}</strong>
											<p>{issue.detail}</p>
											{issue.action !== undefined && <span>{issue.action}</span>}
										</li>
									))}
								</ul>
								<button
									className="primary-action"
									onClick={(): void => void inspectInput()}
								>
									Check again
								</button>
							</div>
						) : summary === null ? (
							<div className="ready-state">
								<div className="readiness-thesis">
									<p className="section-kicker">Current load order</p>
									<h2>{inspection.value.playset?.name ?? "EU4 playset"}</h2>
									<p>
										Analyze every merge unit before any output is written. Foch keeps
										the analyzed result opaque and read-only in this checkpoint.
									</p>
								</div>
								<button
									className="primary-action analyze-action"
									onClick={(): void => void startAnalysis()}
									disabled={!canAnalyze}
								>
									<span>
										{starting ? "Starting analysis…" : "Analyze current playset"}
									</span>
									<small>No mod files will be changed</small>
								</button>
								{analysisError !== null && (
									<p className="error-copy">{analysisError}</p>
								)}
							</div>
						) : (
							<>
								<AnalysisHeader
									summary={summary}
									cancelling={cancelling}
									onCancel={(): void => void cancelAnalysis()}
								/>
								<DispositionRibbon summary={summary} />
								{analysisError !== null && (
									<p className="inline-error" role="alert">
										Analysis update failed: {analysisError}
									</p>
								)}
								<section className="unit-browser" aria-labelledby="unit-browser-title">
									<div className="unit-browser-heading">
										<div>
											<p className="section-kicker">Merge units</p>
											<h2 id="unit-browser-title">
												{pageValue.total.toLocaleString()} results
											</h2>
										</div>
										<form className="unit-tools" onSubmit={submitSearch}>
											<label>
												<span className="sr-only">Search merge units</span>
												<input
													type="search"
													placeholder="Search path or family"
													value={searchDraft}
													onChange={(event): void => setSearchDraft(event.target.value)}
												/>
											</label>
											<button type="submit" className="search-action">
												Search
											</button>
											<label>
												<span className="sr-only">Filter merge units by outcome</span>
												<select
													aria-label="Filter merge units by outcome"
													value={disposition ?? "all"}
													onChange={(event): void => {
														setPage(1);
														setDisposition(
															event.target.value === "all"
																? null
																: (event.target.value as MergeDisposition),
														);
													}}
												>
													<option value="all">All outcomes</option>
													{dispositionOptions.map((option): JSX.Element => (
														<option key={option.value} value={option.value}>
															{option.label}
														</option>
													))}
												</select>
											</label>
										</form>
									</div>
									<div className="unit-list" aria-busy={unitPage.loading}>
										{unitPage.loading && pageValue.items.length === 0 ? (
											<p className="panel-placeholder">Loading merge units…</p>
										) : unitPage.error !== null ? (
											<p className="error-copy">
												Merge units unavailable: {unitPage.error}
											</p>
										) : pageValue.items.length === 0 ? (
											<p className="panel-placeholder">
												No merge units match this view.
											</p>
										) : (
											pageValue.items.map((unit): JSX.Element => (
												<UnitRow
													key={unit.id}
													unit={unit}
													selected={selectedUnitId === unit.id}
													onSelect={setSelectedUnitId}
												/>
											))
										)}
									</div>
									<footer className="pagination" aria-label="Merge unit pages">
										<button
											type="button"
											onClick={(): void =>
												setPage((current): number => Math.max(1, current - 1))
											}
											disabled={page <= 1}
										>
											Previous
										</button>
										<span>
											Page {page} of {pageCount}
										</span>
										<button
											type="button"
											onClick={(): void =>
												setPage((current): number => Math.min(pageCount, current + 1))
											}
											disabled={page >= pageCount}
										>
											Next
										</button>
									</footer>
								</section>
							</>
						)}
					</section>
					{showBrowser ? (
						<UnitDetailPanel detail={unitDetail} />
					) : (
						<aside className="detail-pane detail-empty" aria-label="Merge unit detail">
							<p className="section-kicker">Unit detail</p>
							<h2>Analysis evidence appears here</h2>
							<p>
								Foch exposes focused outcomes and contributors, never raw syntax trees.
							</p>
						</aside>
					)}
				</div>
			)}
		</main>
	);
}
