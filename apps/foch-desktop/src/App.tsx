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
	type AnalysisInputMode,
	type AnalysisInputScope,
	type DesktopClient,
	type InputInspection,
	type InputRecoveryOption,
	type MergeAnalysisStage,
	type MergeAnalysisState,
	type MergeAnalysisSummary,
	type MergeDisposition,
	type MergeUnitDetail,
	type MergeUnitListItem,
	type MergeUnitPage,
	hasMergeReview,
	isAnalysisActive,
	tauriDesktopClient,
} from "./api";

const PAGE_SIZE = 12;
const MAX_QUERY_CHARS: number = 256;
const OMITTED_MOD_PREVIEW_LIMIT: number = 5;

const DISPOSITION_LABELS: Record<MergeDisposition, string> = {
	safe: "Safe merge",
	copy: "Copy through",
	needs_user_choice: "Needs a choice",
	unsupported_input: "Unsupported input",
	engine_failure: "Engine failure",
	deferred: "Deferred",
};

const STAGE_LABELS: Record<MergeAnalysisStage, string> = {
	inventory: "Scan load order",
	resolve_input: "Read playset",
	semantic_merge: "Compare merge units",
	validate_output: "Check results",
	freeze_artifacts: "Prepare review",
	complete: "Complete",
};

const ANALYSIS_STATE_LABELS: Record<MergeAnalysisState, string> = {
	queued: "Queued",
	running: "Analyzing",
	ready: "Review ready",
	ready_with_deferrals: "Review required",
	blocked: "Review blocked",
	cancelled: "Cancelled",
	failed: "Analysis failed",
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

const EMPTY_UNIT_DETAIL: AsyncValue<MergeUnitDetail> = {
	value: null,
	loading: false,
	error: null,
};

function initialAsyncValue<T>(): AsyncValue<T> {
	return { value: null, loading: true, error: null };
}

function queuedAnalysisSummary(
	analysisId: string,
	inputScope: AnalysisInputScope,
): MergeAnalysisSummary {
	return {
		analysisId,
		state: "queued",
		stage: "inventory",
		completedUnits: 0,
		totalUnits: 0,
		elapsedMs: 0,
		counts: {
			total: 0,
			safe: 0,
			copy: 0,
			needsUserChoice: 0,
			unsupportedInput: 0,
			engineFailure: 0,
			deferred: 0,
		},
		message: null,
		inputScope,
	};
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

async function loadInputInspection(
	client: DesktopClient,
): Promise<AsyncValue<InputInspection>> {
	try {
		return {
			value: await client.inspectInput(),
			loading: false,
			error: null,
		};
	} catch (error: unknown) {
		return { value: null, loading: false, error: errorMessage(error) };
	}
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

function hasInputOmissions(scope: AnalysisInputScope): boolean {
	return scope.mode === "without_unavailable_mods";
}

function analysisStateLabel(summary: MergeAnalysisSummary): string {
	if (!hasInputOmissions(summary.inputScope))
		return ANALYSIS_STATE_LABELS[summary.state];
	if (summary.state === "ready") return "Incomplete review ready";
	if (summary.state === "ready_with_deferrals") return "Incomplete review required";
	if (summary.state === "blocked") return "Incomplete review blocked";
	return ANALYSIS_STATE_LABELS[summary.state];
}

function StatusMark({ disposition }: { disposition: MergeDisposition }): JSX.Element {
	return (
		<span className={`status-mark ${dispositionClass(disposition)}`}>
			<span className="status-mark-dot" aria-hidden="true" />
			{DISPOSITION_LABELS[disposition]}
		</span>
	);
}

function ReadinessRail({
	inspection,
	inputScope,
}: {
	inspection: InputInspection;
	inputScope: AnalysisInputScope | null;
}): JSX.Element {
	const enabledMods =
		inspection.playset?.mods.filter((mod): boolean => mod.enabled) ?? [];
	const scope = inputScope ?? inspection.recovery;
	const omittedModIds: Set<string> = new Set(
		scope?.omittedMods.map((mod): string => mod.id) ?? [],
	);
	const omissionsAccepted: boolean =
		inputScope !== null && hasInputOmissions(inputScope);
	const readiness = omissionsAccepted ? "ready_with_omissions" : inspection.readiness;
	const readinessCopy: string = omissionsAccepted
		? "Incomplete input"
		: readiness === "ready"
			? "Ready to analyze"
			: readiness === "ready_with_omissions"
				? "Available mods ready"
				: "Setup blocked";
	const readinessSymbol: string =
		readiness === "ready" ? "✓" : readiness === "blocked" ? "!" : "–";
	const modCountCopy: string =
		scope === null
			? enabledMods.length.toString()
			: `${scope.includedModCount}/${scope.sourceModCount}`;

	return (
		<aside className="input-rail" aria-label="Detected EU4 input">
			<div className="rail-section rail-readiness">
				<p className="section-kicker">Input readiness</p>
				<div className={`readiness-seal readiness-${readiness}`}>
					<span aria-hidden="true">{readinessSymbol}</span>
					<strong>{readinessCopy}</strong>
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
						<h2 title={inspection.playset?.name ?? undefined}>
							{inspection.playset?.name ?? "Not detected"}
						</h2>
					</div>
					<span
						className="compact-count"
						title={
							scope === null ? "Enabled mods" : "Included mods / source playset mods"
						}
					>
						{modCountCopy}
					</span>
				</div>

				{inspection.playset === null ? (
					<p className="rail-empty">No current EU4 playset was found.</p>
				) : (
					<ol className="load-order" aria-label="Detected mod load order">
						{inspection.playset.mods.map((mod): JSX.Element => {
							const unavailable: boolean =
								omittedModIds.has(mod.id) || mod.sourceError !== null;
							const className: string = [
								!mod.enabled ? "mod-disabled" : "",
								unavailable ? "mod-unavailable" : "",
							]
								.filter(Boolean)
								.join(" ");
							return (
								<li key={mod.id} className={className}>
									<span className="load-position">
										{mod.position.toString().padStart(2, "0")}
									</span>
									<span className="mod-copy">
										<span className="mod-name" title={mod.name}>
											{mod.name}
										</span>
										<span
											className="mod-identity"
											title={mod.descriptorPath ?? undefined}
										>
											{mod.workshopId === null
												? "Local mod"
												: `Workshop ${mod.workshopId}`}
											{mod.version !== null && ` · v${mod.version}`}
											{mod.workshopManifestId !== null &&
												` · manifest ${mod.workshopManifestId}`}
										</span>
										{mod.declaredDependencies.length > 0 && (
											<span
												className="mod-dependencies"
												title={mod.declaredDependencies.join(", ")}
											>
												Depends on: {mod.declaredDependencies.join(", ")}
											</span>
										)}
										{mod.sourceError !== null && (
											<span className="mod-source-error" title={mod.sourceError}>
												Unavailable: {mod.sourceError}
											</span>
										)}
									</span>
									{unavailable ? (
										<span className="mod-state">Unavailable</span>
									) : (
										!mod.enabled && <span className="mod-state">Off</span>
									)}
								</li>
							);
						})}
					</ol>
				)}
			</div>
		</aside>
	);
}

function RecoveryState({
	recovery,
	checking,
	starting,
	error,
	onAnalyze,
	onCheck,
}: {
	recovery: InputRecoveryOption;
	checking: boolean;
	starting: boolean;
	error: string | null;
	onAnalyze: () => void;
	onCheck: () => void;
}): JSX.Element {
	const omittedPreview = recovery.omittedMods.slice(0, OMITTED_MOD_PREVIEW_LIMIT);
	const remainingOmittedCount: number = Math.max(
		0,
		recovery.omittedModCount - omittedPreview.length,
	);
	const modNoun: string = recovery.omittedModCount === 1 ? "mod" : "mods";

	return (
		<div className="recovery-state">
			<p className="section-kicker">Incomplete playset</p>
			<h2>
				{recovery.omittedModCount} Workshop {modNoun} unavailable
			</h2>
			<p className="state-intro">
				Foch can analyze {recovery.includedModCount} of {recovery.sourceModCount} mods.
				The Launcher playset and every mod file stay untouched.
			</p>
			<ul className="omission-list" aria-label="Unavailable Workshop mods">
				{omittedPreview.map((mod): JSX.Element => (
					<li key={mod.id}>
						<span>{mod.position.toString().padStart(2, "0")}</span>
						<div>
							<strong>{mod.name}</strong>
							<small>{mod.reason}</small>
						</div>
					</li>
				))}
				{remainingOmittedCount > 0 && (
					<li className="omission-overflow">
						<span>+</span>
						<div>
							<strong>{remainingOmittedCount} more unavailable</strong>
						</div>
					</li>
				)}
			</ul>
			<p className="recovery-warning">
				The review will be marked incomplete because unavailable mods cannot contribute
				to its outcomes.
			</p>
			<div className="recovery-actions">
				<button
					className="primary-action analyze-action"
					type="button"
					onClick={onAnalyze}
					disabled={checking || starting}
				>
					<span>{starting ? "Starting analysis…" : "Analyze available mods"}</span>
					<small>Results will be marked incomplete</small>
				</button>
				<button
					className="secondary-action"
					type="button"
					onClick={onCheck}
					disabled={checking || starting}
				>
					{checking ? "Checking…" : "Check again"}
				</button>
			</div>
			{error !== null && (
				<p className="error-copy" role="alert">
					{error}
				</p>
			)}
		</div>
	);
}

function InputScopeNotice({
	scope,
}: {
	scope: AnalysisInputScope;
}): JSX.Element | null {
	if (!hasInputOmissions(scope)) return null;
	const modNoun: string = scope.omittedModCount === 1 ? "mod" : "mods";
	const omittedNames: string = scope.omittedMods
		.map((mod): string => mod.name)
		.join(", ");

	return (
		<section className="input-scope-notice" role="status" aria-label="Incomplete input">
			<span className="input-scope-mark" aria-hidden="true" />
			<div>
				<p className="section-kicker">Incomplete input</p>
				<strong title={omittedNames || undefined}>
					{scope.includedModCount} of {scope.sourceModCount} playset mods analyzed
				</strong>
				<p>
					{scope.omittedModCount} unavailable {modNoun} omitted. Outcomes apply only to
					the included mods; the Launcher playset remains unchanged.
				</p>
			</div>
		</section>
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
					<h2>{analysisStateLabel(summary)}</h2>
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

function ReviewUnavailablePanel({
	state,
	inputScope,
	onReset,
}: {
	state: MergeAnalysisState;
	inputScope: AnalysisInputScope;
	onReset: () => void;
}): JSX.Element {
	const active: boolean = isAnalysisActive(state);
	const incomplete: boolean = hasInputOmissions(inputScope);
	const copy: { title: string; detail: string } = active
		? {
				title: incomplete
					? "Building the available-mod review"
					: "Building the complete review",
				detail: incomplete
					? "Results appear after every included merge unit has reached a final outcome. Unavailable mods are not represented."
					: "Results appear after every merge unit has reached a final outcome. No partial review is shown.",
			}
		: {
				title: "No review was produced",
				detail:
					state === "cancelled"
						? "The analysis stopped before it produced final outcomes. Check the current input before starting again."
						: "The analysis ended before a review was available. Check the current input, then run it again.",
			};

	return (
		<section className="review-unavailable" aria-live="polite">
			<span
				className={`review-state-mark ${active ? "review-state-active" : ""}`}
				aria-hidden="true"
			/>
			<div>
				<p className="section-kicker">Merge unit review</p>
				<h2>{copy.title}</h2>
				<p>{copy.detail}</p>
				{!active && (
					<button className="primary-action" type="button" onClick={onReset}>
						Check input again
					</button>
				)}
			</div>
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
			title={unit.path}
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
	const [unitDetail, setUnitDetail] =
		useState<AsyncValue<MergeUnitDetail>>(EMPTY_UNIT_DETAIL);
	const [searchDraft, setSearchDraft] = useState<string>("");
	const [searchQuery, setSearchQuery] = useState<string>("");
	const [disposition, setDisposition] = useState<MergeDisposition | null>(null);
	const [page, setPage] = useState<number>(1);

	const inspectInput = useCallback(async (): Promise<void> => {
		setAnalysisError(null);
		setInspection((current): AsyncValue<InputInspection> => ({
			value: current.value,
			loading: true,
			error: null,
		}));
		setInspection(await loadInputInspection(client));
	}, [client]);

	useEffect((): (() => void) => {
		let disposed = false;
		void loadInputInspection(client).then(
			(value: AsyncValue<InputInspection>): void => {
				if (!disposed) setInspection(value);
			},
		);
		return (): void => {
			disposed = true;
		};
	}, [client]);

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
				if (disposed) return;
				setAnalysisError(errorMessage(error));
				timer = setTimeout((): void => {
					void refresh();
				}, pollIntervalMs);
			}
		};
		void refresh();
		return (): void => {
			disposed = true;
			if (timer !== undefined) clearTimeout(timer);
		};
	}, [analysisId, client, pollIntervalMs]);

	const reviewAvailable: boolean = summary !== null && hasMergeReview(summary.state);

	useEffect((): (() => void) | undefined => {
		if (analysisId === null || !reviewAvailable) return undefined;
		let disposed = false;
		queueMicrotask((): void => {
			if (disposed) return;
			setUnitPage((current): AsyncValue<MergeUnitPage> => ({
				value: current.value,
				loading: true,
				error: null,
			}));
		});
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
				if (value.page !== page) setPage(value.page);
				setSelectedUnitId((current): string | null => {
					if (
						current !== null &&
						value.items.some((item): boolean => item.id === current)
					)
						return current;
					return null;
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
	}, [analysisId, client, disposition, page, reviewAvailable, searchQuery]);

	useEffect((): (() => void) => {
		let disposed = false;
		if (analysisId === null || selectedUnitId === null || !reviewAvailable) {
			queueMicrotask((): void => {
				if (!disposed) setUnitDetail({ value: null, loading: false, error: null });
			});
			return (): void => {
				disposed = true;
			};
		}
		queueMicrotask((): void => {
			if (!disposed) setUnitDetail({ value: null, loading: true, error: null });
		});
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
	}, [analysisId, client, reviewAvailable, selectedUnitId]);

	const startAnalysis = async (inputMode: AnalysisInputMode): Promise<void> => {
		if (inspection.value === null) return;
		setStarting(true);
		setAnalysisError(null);
		setSummary(null);
		setSelectedUnitId(null);
		setUnitDetail(EMPTY_UNIT_DETAIL);
		setUnitPage({ value: EMPTY_UNIT_PAGE, loading: false, error: null });
		try {
			const started = await client.startMergeAnalysis(
				inspection.value.inspectionId,
				inputMode,
			);
			setAnalysisId(started.analysisId);
			setSummary(queuedAnalysisSummary(started.analysisId, started.inputScope));
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

	const resetAnalysis = async (): Promise<void> => {
		setAnalysisId(null);
		setSummary(null);
		setAnalysisError(null);
		setSelectedUnitId(null);
		setUnitDetail(EMPTY_UNIT_DETAIL);
		setUnitPage({ value: EMPTY_UNIT_PAGE, loading: false, error: null });
		setSearchDraft("");
		setSearchQuery("");
		setDisposition(null);
		setPage(1);
		await inspectInput();
	};

	const submitSearch = (event: FormEvent<HTMLFormElement>): void => {
		event.preventDefault();
		setPage(1);
		setSearchQuery(searchDraft.trim());
	};

	const pageValue = unitPage.value ?? EMPTY_UNIT_PAGE;
	const pageCount: number = Math.max(
		1,
		Math.ceil(pageValue.total / Math.max(1, pageValue.pageSize)),
	);
	const displayedPage: number = Math.min(Math.max(1, pageValue.page), pageCount);
	const playsetName = inspection.value?.playset?.name ?? "Current EU4 playset";
	const inputRecovery: InputRecoveryOption | null = inspection.value?.recovery ?? null;
	const canAnalyze =
		inspection.value?.readiness === "ready" &&
		!inspection.loading &&
		!starting &&
		analysisId === null;
	const showBrowser = analysisId !== null && reviewAvailable;
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
				<span className="read-only-badge">Read-only analysis</span>
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
					<ReadinessRail
						inspection={inspection.value}
						inputScope={summary?.inputScope ?? null}
					/>
					<section className="analysis-workspace" aria-label="Merge analysis workspace">
						{summary === null &&
						inspection.value.readiness === "ready_with_omissions" &&
						inputRecovery !== null ? (
							<RecoveryState
								recovery={inputRecovery}
								checking={inspection.loading}
								starting={starting}
								error={analysisError}
								onAnalyze={(): void => void startAnalysis(inputRecovery.kind)}
								onCheck={(): void => void inspectInput()}
							/>
						) : summary === null && inspection.value.readiness === "blocked" ? (
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
									disabled={inspection.loading}
								>
									{inspection.loading ? "Checking…" : "Check again"}
								</button>
							</div>
						) : summary === null ? (
							<div className="ready-state">
								<div className="readiness-thesis">
									<p className="section-kicker">Current load order</p>
									<h2>{inspection.value.playset?.name ?? "EU4 playset"}</h2>
									<p>
										Foch reads the detected load order and prepares a complete review.
										It does not change Launcher settings, game files, or mod files.
									</p>
								</div>
								<button
									className="primary-action analyze-action"
									onClick={(): void => void startAnalysis("complete")}
									disabled={!canAnalyze}
								>
									<span>
										{inspection.loading
											? "Checking input…"
											: starting
												? "Starting analysis…"
												: "Analyze current playset"}
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
								<InputScopeNotice scope={summary.inputScope} />
								{analysisError !== null && (
									<p className="inline-error" role="alert">
										Analysis update failed: {analysisError}
									</p>
								)}
								{reviewAvailable && <DispositionRibbon summary={summary} />}
								{!reviewAvailable && (
									<ReviewUnavailablePanel
										state={summary.state}
										inputScope={summary.inputScope}
										onReset={(): void => void resetAnalysis()}
									/>
								)}
								<section
									className="unit-browser"
									aria-labelledby="unit-browser-title"
									hidden={!reviewAvailable}
								>
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
													maxLength={MAX_QUERY_CHARS}
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
											onClick={(): void => setPage(Math.max(1, displayedPage - 1))}
											disabled={unitPage.loading || displayedPage <= 1}
										>
											Previous
										</button>
										<span>
											Page {displayedPage} of {pageCount}
										</span>
										<button
											type="button"
											onClick={(): void =>
												setPage(Math.min(pageCount, displayedPage + 1))
											}
											disabled={unitPage.loading || displayedPage >= pageCount}
										>
											Next
										</button>
									</footer>
								</section>
							</>
						)}
					</section>
					{showBrowser ? (
						<UnitDetailPanel
							detail={selectedUnitId === null ? EMPTY_UNIT_DETAIL : unitDetail}
						/>
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
