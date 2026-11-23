import "./App.css";

import {
	type CSSProperties,
	type FormEvent,
	type JSX,
	createContext,
	useCallback,
	useContext,
	useEffect,
	useMemo,
	useState,
} from "react";

import {
	MERGE_DISPOSITIONS,
	type AnalysisInputMode,
	type AnalysisInputScope,
	type DesktopClient,
	type DesktopPluginOverview,
	type DesktopPreferences,
	type EffectiveLanguage,
	type InputInspection,
	type InputRecoveryOption,
	type MergeAnalysisSummary,
	type MergeAnalysisState,
	type MergeDisposition,
	type MergeUnitDetail,
	type MergeUnitListItem,
	type MergeUnitPage,
	hasMergeReview,
	isAnalysisActive,
	parseDesktopCommandError,
	tauriDesktopClient,
} from "./api";
import { type Copy, copyFor } from "./i18n";
import { PlayPanel, PluginsPanel, SettingsPanel } from "./panels";

const CopyContext = createContext<Copy>(copyFor("en"));

function useCopy(): Copy {
	return useContext(CopyContext);
}

type DesktopTab = "play" | "plugins" | "merge" | "settings";

const PAGE_SIZE = 12;
const MAX_QUERY_CHARS: number = 256;
const OMITTED_MOD_PREVIEW_LIMIT: number = 5;
const MAX_CONSECUTIVE_POLL_FAILURES: number = 3;

interface AppProps {
	client?: DesktopClient;
	pollIntervalMs?: number;
}

type AnalysisUpdateStatus = "polling" | "paused" | "unavailable";

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

function queuedAnalysisSummary(analysisId: string): MergeAnalysisSummary {
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
	};
}

function errorMessage(error: unknown): string {
	return parseDesktopCommandError(error).message;
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

function analysisStateLabel(
	summary: MergeAnalysisSummary,
	inputScope: AnalysisInputScope,
	copy: Copy,
): string {
	if (!hasInputOmissions(inputScope)) return copy.merge.state[summary.state];
	if (summary.state === "ready") return copy.merge.incompleteReady;
	if (summary.state === "ready_with_deferrals") return copy.merge.incompleteRequired;
	if (summary.state === "blocked") return copy.merge.incompleteBlocked;
	return copy.merge.state[summary.state];
}

function StatusMark({ disposition }: { disposition: MergeDisposition }): JSX.Element {
	const copy = useCopy();
	return (
		<span className={`status-mark ${dispositionClass(disposition)}`}>
			<span className="status-mark-dot" aria-hidden="true" />
			{copy.merge.disposition[disposition]}
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
	const copy = useCopy();
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
		? copy.merge.incomplete
		: readiness === "ready"
			? copy.merge.ready
			: readiness === "ready_with_omissions"
				? copy.merge.availableReady
				: copy.merge.blocked;
	const readinessSymbol: string =
		readiness === "ready" ? "✓" : readiness === "blocked" ? "!" : "–";
	const modCountCopy: string =
		scope === null
			? enabledMods.length.toString()
			: `${scope.includedModCount}/${scope.sourceModCount}`;

	return (
		<aside className="input-rail" aria-label={copy.merge.detectedInput}>
			<div className="rail-section rail-readiness">
				<p className="section-kicker">{copy.merge.inputReadiness}</p>
				<div className={`readiness-seal readiness-${readiness}`}>
					<span aria-hidden="true">{readinessSymbol}</span>
					<strong>{readinessCopy}</strong>
				</div>
			</div>

			<div className="rail-section facts-list">
				<div>
					<span>{copy.merge.game}</span>
					<strong>{inspection.game.name}</strong>
				</div>
				<div>
					<span>Version</span>
					<strong>{inspection.game.version ?? copy.merge.versionMissing}</strong>
				</div>
				<div>
					<span>{copy.merge.base}</span>
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
						<p className="section-kicker">{copy.merge.currentPlayset}</p>
						<h2 title={inspection.playset?.name ?? undefined}>
							{inspection.playset?.name ?? copy.merge.notDetected}
						</h2>
					</div>
					<span
						className="compact-count"
						title={scope === null ? copy.merge.enabledMods : copy.merge.includedMods}
					>
						{modCountCopy}
					</span>
				</div>

				{inspection.playset === null ? (
					<p className="rail-empty">{copy.merge.noPlayset}</p>
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
												? copy.merge.localMod
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
												{copy.merge.dependsOn} {mod.declaredDependencies.join(", ")}
											</span>
										)}
										{mod.sourceError !== null && (
											<span className="mod-source-error" title={mod.sourceError}>
												{copy.merge.unavailablePrefix} {mod.sourceError}
											</span>
										)}
									</span>
									{unavailable ? (
										<span className="mod-state">{copy.merge.unavailable}</span>
									) : (
										!mod.enabled && <span className="mod-state">{copy.merge.off}</span>
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
	const copy = useCopy();
	const omittedPreview = recovery.omittedMods.slice(0, OMITTED_MOD_PREVIEW_LIMIT);
	const remainingOmittedCount: number = Math.max(
		0,
		recovery.omittedModCount - omittedPreview.length,
	);
	return (
		<div className="recovery-state">
			<p className="section-kicker">{copy.merge.incompleteKicker}</p>
			<h2>
				{recovery.omittedModCount}{" "}
				{recovery.omittedModCount === 1
					? copy.merge.workshopUnavailable
					: copy.merge.workshopUnavailablePlural}
			</h2>
			<p className="state-intro">
				Foch can analyze {recovery.includedModCount} of {recovery.sourceModCount} mods.{" "}
				{copy.merge.launcherUntouched}
			</p>
			<ul className="omission-list" aria-label={copy.merge.omittedLabel}>
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
							<strong>
								{remainingOmittedCount} {copy.merge.moreUnavailable}
							</strong>
						</div>
					</li>
				)}
			</ul>
			<p className="recovery-warning">{copy.merge.incompleteWarning}</p>
			<div className="recovery-actions">
				<button
					className="primary-action analyze-action"
					type="button"
					onClick={onAnalyze}
					disabled={checking || starting}
				>
					<span>{starting ? copy.merge.starting : copy.merge.analyzeAvailable}</span>
					<small>{copy.merge.analyzeAvailableHint}</small>
				</button>
				<button
					className="secondary-action"
					type="button"
					onClick={onCheck}
					disabled={checking || starting}
				>
					{checking ? copy.merge.checking : copy.merge.checkAgain}
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
	const copy = useCopy();
	if (!hasInputOmissions(scope)) return null;
	const modNoun: string = scope.omittedModCount === 1 ? "mod" : "mods";
	const omittedNames: string = scope.omittedMods
		.map((mod): string => mod.name)
		.join(", ");

	return (
		<section
			className="input-scope-notice"
			role="status"
			aria-label={copy.merge.incomplete}
		>
			<span className="input-scope-mark" aria-hidden="true" />
			<div>
				<p className="section-kicker">{copy.merge.incomplete}</p>
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
	const copy = useCopy();
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
						<span>{copy.merge.disposition[disposition]}</span>
						<strong>{counts[disposition]}</strong>
					</div>
				))}
			</div>
		</div>
	);
}

function AnalysisHeader({
	summary,
	inputScope,
	cancelling,
	onCancel,
}: {
	summary: MergeAnalysisSummary;
	inputScope: AnalysisInputScope;
	cancelling: boolean;
	onCancel: () => void;
}): JSX.Element {
	const copy = useCopy();
	const progress =
		summary.totalUnits === 0
			? 0
			: Math.min(100, Math.round((summary.completedUnits / summary.totalUnits) * 100));
	const active = isAnalysisActive(summary.state);

	return (
		<section className="analysis-header" aria-live="polite">
			<div className="analysis-title-row">
				<div>
					<p className="section-kicker">{copy.merge.readOnly}</p>
					<h2>{analysisStateLabel(summary, inputScope, copy)}</h2>
				</div>
				{active && (
					<button
						className="secondary-action danger-action"
						onClick={onCancel}
						disabled={cancelling}
					>
						{cancelling ? copy.merge.cancelling : copy.merge.cancel}
					</button>
				)}
			</div>
			<div className="progress-copy">
				<strong>{copy.merge.stage[summary.stage]}</strong>
				<span>
					{summary.completedUnits.toLocaleString()} /{" "}
					{summary.totalUnits.toLocaleString()} {copy.merge.units}
				</span>
				<span>
					{formatElapsed(summary.elapsedMs)} {copy.merge.elapsed}
				</span>
			</div>
			<div
				className="progress-track"
				role="progressbar"
				aria-label={copy.merge.progress}
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
	const copy = useCopy();
	const active: boolean = isAnalysisActive(state);
	const incomplete: boolean = hasInputOmissions(inputScope);
	const panelCopy: { title: string; detail: string } = active
		? {
				title: incomplete ? copy.merge.buildingAvailable : copy.merge.buildingComplete,
				detail: incomplete
					? copy.merge.buildingAvailableDetail
					: copy.merge.buildingCompleteDetail,
			}
		: {
				title: copy.merge.noReview,
				detail:
					state === "cancelled" ? copy.merge.cancelledDetail : copy.merge.endedDetail,
			};

	return (
		<section className="review-unavailable" aria-live="polite">
			<span
				className={`review-state-mark ${active ? "review-state-active" : ""}`}
				aria-hidden="true"
			/>
			<div>
				<p className="section-kicker">{copy.merge.reviewKicker}</p>
				<h2>{panelCopy.title}</h2>
				<p>{panelCopy.detail}</p>
				{!active && (
					<button className="primary-action" type="button" onClick={onReset}>
						{copy.merge.checkInputAgain}
					</button>
				)}
			</div>
		</section>
	);
}

function AnalysisUnavailablePanel({
	message,
	onReset,
}: {
	message: string | null;
	onReset: () => void;
}): JSX.Element {
	const copy = useCopy();
	return (
		<section className="review-unavailable" role="alert">
			<span className="review-state-mark" aria-hidden="true" />
			<div>
				<p className="section-kicker">{copy.merge.readOnly}</p>
				<h2>{copy.merge.analysisUnavailable}</h2>
				<p>{message ?? copy.merge.analysisExpired}</p>
				<button className="primary-action" type="button" onClick={onReset}>
					{copy.merge.checkInputAgain}
				</button>
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
	const copy = useCopy();
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
				{unit.kind === "definition_module"
					? copy.merge.definitionModule
					: copy.merge.file}{" "}
				· {unit.contributorCount} {copy.merge.sources}
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
	const copy = useCopy();
	if (detail.loading) {
		return (
			<aside className="detail-pane" aria-label={copy.merge.unitDetail}>
				<p className="panel-placeholder">{copy.merge.loadingDetail}</p>
			</aside>
		);
	}
	if (detail.error !== null) {
		return (
			<aside className="detail-pane" aria-label={copy.merge.unitDetail}>
				<p className="error-copy">
					{copy.merge.detailUnavailable} {detail.error}
				</p>
			</aside>
		);
	}
	if (detail.value === null) {
		return (
			<aside className="detail-pane detail-empty" aria-label={copy.merge.unitDetail}>
				<p className="section-kicker">{copy.merge.unitDetail}</p>
				<h2>{copy.merge.selectUnit}</h2>
				<p>{copy.merge.selectUnitBody}</p>
			</aside>
		);
	}

	const unit = detail.value;
	return (
		<aside className="detail-pane" aria-label={copy.merge.unitDetail}>
			<div className="detail-heading">
				<p className="section-kicker">{copy.merge.selectedUnit}</p>
				<h2>{unit.path}</h2>
				<StatusMark disposition={unit.disposition} />
			</div>
			<dl className="detail-facts">
				<div>
					<dt>{copy.merge.family}</dt>
					<dd>{unit.family}</dd>
				</div>
				<div>
					<dt>{copy.merge.strategy}</dt>
					<dd>{unit.strategy}</dd>
				</div>
				<div>
					<dt>{copy.merge.unit}</dt>
					<dd>
						{unit.kind === "definition_module"
							? copy.merge.definitionModule
							: copy.merge.file}
					</dd>
				</div>
				{unit.outputPath !== null && (
					<div>
						<dt>{copy.merge.output}</dt>
						<dd className="path-value">{unit.outputPath}</dd>
					</div>
				)}
			</dl>
			<section className="detail-section">
				<h3>{copy.merge.outcome}</h3>
				<p>{unit.summary}</p>
			</section>
			<section className="detail-section">
				<h3>{copy.merge.contributors}</h3>
				<ol className="contributor-list">
					{unit.contributors.map((contributor): JSX.Element => (
						<li key={`${contributor.modId}-${contributor.position}`}>
							<span>{contributor.position.toString().padStart(2, "0")}</span>
							<strong>{contributor.name}</strong>
							{contributor.isBaseGame && <small>{copy.merge.baseGame}</small>}
						</li>
					))}
				</ol>
			</section>
			{unit.notes.length > 0 && (
				<section className="detail-section">
					<h3>{copy.merge.notes}</h3>
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
	const [analysisInputScope, setAnalysisInputScope] =
		useState<AnalysisInputScope | null>(null);
	const [summary, setSummary] = useState<MergeAnalysisSummary | null>(null);
	const [analysisError, setAnalysisError] = useState<string | null>(null);
	const [analysisUpdateStatus, setAnalysisUpdateStatus] =
		useState<AnalysisUpdateStatus>("polling");
	const [pollRetryGeneration, setPollRetryGeneration] = useState<number>(0);
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
	const [tab, setTab] = useState<DesktopTab>("play");
	const [language, setLanguage] = useState<EffectiveLanguage>("en");
	const [preferences, setPreferences] = useState<DesktopPreferences | null>(null);
	const [overview, setOverview] =
		useState<AsyncValue<DesktopPluginOverview>>(
			initialAsyncValue<DesktopPluginOverview>(),
		);
	const copy = copyFor(language);
	const playsetNameForPlugins =
		inspection.value?.playset?.name ?? overview.value?.playset ?? null;

	const reloadOverview = useCallback(async (): Promise<void> => {
		setOverview((current): AsyncValue<DesktopPluginOverview> => ({
			value: current.value,
			loading: true,
			error: null,
		}));
		try {
			setOverview({
				value: await client.getPluginOverview(playsetNameForPlugins),
				loading: false,
				error: null,
			});
		} catch (error: unknown) {
			setOverview({ value: null, loading: false, error: errorMessage(error) });
		}
	}, [client, playsetNameForPlugins]);

	useEffect((): (() => void) => {
		let disposed = false;
		void client.getDesktopSettings().then(
			(value: DesktopPreferences): void => {
				if (disposed) return;
				setPreferences(value);
				setLanguage(value.effectiveLanguage);
			},
			(): void => {
				if (!disposed) setPreferences(null);
			},
		);
		return (): void => {
			disposed = true;
		};
	}, [client]);

	useEffect((): (() => void) => {
		let disposed = false;
		void client.getPluginOverview(playsetNameForPlugins).then(
			(value: DesktopPluginOverview): void => {
				if (!disposed) setOverview({ value, loading: false, error: null });
			},
			(error: unknown): void => {
				if (!disposed)
					setOverview({ value: null, loading: false, error: errorMessage(error) });
			},
		);
		return (): void => {
			disposed = true;
		};
	}, [client, playsetNameForPlugins]);

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
		let consecutiveFailures = 0;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const refresh = async (): Promise<void> => {
			try {
				const next = await client.getMergeAnalysisSummary(analysisId);
				if (disposed) return;
				consecutiveFailures = 0;
				setSummary(next);
				setAnalysisError(null);
				setAnalysisUpdateStatus("polling");
				if (isAnalysisActive(next.state)) {
					timer = setTimeout((): void => {
						void refresh();
					}, pollIntervalMs);
				}
			} catch (error: unknown) {
				if (disposed) return;
				const commandError = parseDesktopCommandError(error);
				setAnalysisError(commandError.message);
				if (commandError.code === "analysis_not_found") {
					setAnalysisUpdateStatus("unavailable");
					return;
				}

				consecutiveFailures += 1;
				if (consecutiveFailures >= MAX_CONSECUTIVE_POLL_FAILURES) {
					setAnalysisUpdateStatus("paused");
					return;
				}
				const retryDelayMs: number = pollIntervalMs * 2 ** (consecutiveFailures - 1);
				timer = setTimeout((): void => {
					void refresh();
				}, retryDelayMs);
			}
		};
		void refresh();
		return (): void => {
			disposed = true;
			if (timer !== undefined) clearTimeout(timer);
		};
	}, [analysisId, client, pollIntervalMs, pollRetryGeneration]);

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
		setAnalysisInputScope(null);
		setSummary(null);
		setAnalysisUpdateStatus("polling");
		setPollRetryGeneration(0);
		setSelectedUnitId(null);
		setUnitDetail(EMPTY_UNIT_DETAIL);
		setUnitPage({ value: EMPTY_UNIT_PAGE, loading: false, error: null });
		try {
			const started = await client.startMergeAnalysis(
				inspection.value.inspectionId,
				inputMode,
			);
			setAnalysisInputScope(started.inputScope);
			setAnalysisId(started.analysisId);
			setSummary(queuedAnalysisSummary(started.analysisId));
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
		setAnalysisInputScope(null);
		setSummary(null);
		setAnalysisError(null);
		setAnalysisUpdateStatus("polling");
		setPollRetryGeneration(0);
		setSelectedUnitId(null);
		setUnitDetail(EMPTY_UNIT_DETAIL);
		setUnitPage({ value: EMPTY_UNIT_PAGE, loading: false, error: null });
		setSearchDraft("");
		setSearchQuery("");
		setDisposition(null);
		setPage(1);
		await inspectInput();
	};

	const retryAnalysisUpdate = (): void => {
		setAnalysisError(null);
		setAnalysisUpdateStatus("polling");
		setPollRetryGeneration((generation): number => generation + 1);
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
				label: copy.merge.disposition[value],
			})),
		[copy],
	);
	const mergeAttention =
		(inspection.value?.recovery?.omittedModCount ??
			inspection.value?.playset?.mods.filter((mod): boolean => mod.sourceError !== null)
				.length ??
			0) + (summary?.counts.needsUserChoice ?? 0);

	return (
		<CopyContext.Provider value={copy}>
			<div className="app-frame">
				<nav className="side-nav" aria-label="Foch">
					<div className="brand-lockup">
						<span className="brand-monogram" aria-hidden="true">
							F
						</span>
						<div>
							<h1>Foch</h1>
							<p>{copy.tabs.play}</p>
						</div>
					</div>
					<div className="nav-tabs" role="tablist">
						{(
							[
								["play", copy.tabs.play],
								["plugins", copy.tabs.plugins],
								["merge", copy.tabs.merge],
								["settings", copy.tabs.settings],
							] as const
						).map(([id, label]): JSX.Element => (
							<button
								key={id}
								type="button"
								role="tab"
								id={`tab-${id}`}
								aria-selected={tab === id}
								className={tab === id ? "nav-tab nav-tab-active" : "nav-tab"}
								onClick={(): void => setTab(id)}
							>
								{label}
								{id === "merge" && mergeAttention > 0 && (
									<span className="nav-badge">{mergeAttention}</span>
								)}
							</button>
						))}
					</div>
				</nav>
				<main className="app-shell">
					{tab === "play" && (
						<PlayPanel
							client={client}
							playset={playsetNameForPlugins}
							overview={overview}
							copy={copy}
							onReload={(): void => void reloadOverview()}
						/>
					)}
					{tab === "plugins" && (
						<PluginsPanel
							client={client}
							playset={playsetNameForPlugins}
							overview={overview}
							copy={copy}
							onReload={(): void => void reloadOverview()}
						/>
					)}
					{tab === "settings" && (
						<SettingsPanel
							client={client}
							preferences={preferences}
							copy={copy}
							onPreferences={setPreferences}
							onLanguage={setLanguage}
						/>
					)}
					{tab === "merge" && (
						<>
							<header className="app-header">
								<div className="header-context">
									<span>{copy.merge.detectedInput}</span>
									<strong>{playsetName}</strong>
								</div>
								<span className="read-only-badge">{copy.merge.readOnlyBadge}</span>
							</header>

							{inspection.loading && inspection.value === null ? (
								<section className="launch-state" aria-live="polite">
									<span className="scan-line" aria-hidden="true" />
									<p className="section-kicker">{copy.merge.checkingKicker}</p>
									<h2>{copy.merge.checkingTitle}</h2>
									<p>{copy.merge.checkingBody}</p>
								</section>
							) : inspection.error !== null ? (
								<section className="launch-state launch-error" role="alert">
									<p className="section-kicker">{copy.merge.inspectFailedKicker}</p>
									<h2>{copy.merge.inspectFailedTitle}</h2>
									<p>{inspection.error}</p>
									<button
										className="primary-action"
										onClick={(): void => void inspectInput()}
									>
										{copy.merge.checkAgain}
									</button>
								</section>
							) : inspection.value === null ? null : (
								<div className="workbench">
									<ReadinessRail
										inspection={inspection.value}
										inputScope={analysisInputScope}
									/>
									<section
										className="analysis-workspace"
										aria-label="Merge analysis workspace"
									>
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
												<p className="section-kicker">{copy.merge.actionRequired}</p>
												<h2>{copy.merge.resolveBlockers}</h2>
												<p className="state-intro">{copy.merge.noInference}</p>
												<ul className="issue-list">
													{inspection.value.issues.map((issue): JSX.Element => (
														<li key={issue.id}>
															<strong>{issue.title}</strong>
															<p>{issue.detail}</p>
															{issue.action !== undefined && (
																<span>{issue.action}</span>
															)}
														</li>
													))}
												</ul>
												<button
													className="primary-action"
													onClick={(): void => void inspectInput()}
													disabled={inspection.loading}
												>
													{inspection.loading
														? copy.merge.checking
														: copy.merge.checkAgain}
												</button>
											</div>
										) : summary === null ? (
											<div className="ready-state">
												<div className="readiness-thesis">
													<p className="section-kicker">
														{copy.merge.currentLoadOrder}
													</p>
													<h2>{inspection.value.playset?.name ?? "EU4 playset"}</h2>
													<p>{copy.merge.readOnlyBody}</p>
												</div>
												<button
													className="primary-action analyze-action"
													onClick={(): void => void startAnalysis("complete")}
													disabled={!canAnalyze}
												>
													<span>
														{inspection.loading
															? copy.merge.checking
															: starting
																? copy.merge.starting
																: copy.merge.analyzeCurrent}
													</span>
													<small>{copy.merge.analyzeCurrentHint}</small>
												</button>
												{analysisError !== null && (
													<p className="error-copy">{analysisError}</p>
												)}
											</div>
										) : analysisInputScope === null ? null : analysisUpdateStatus ===
										  "unavailable" ? (
											<AnalysisUnavailablePanel
												message={analysisError}
												onReset={(): void => void resetAnalysis()}
											/>
										) : (
											<>
												<AnalysisHeader
													summary={summary}
													inputScope={analysisInputScope}
													cancelling={cancelling}
													onCancel={(): void => void cancelAnalysis()}
												/>
												<InputScopeNotice scope={analysisInputScope} />
												{analysisError !== null && (
													<div className="inline-error" role="alert">
														<span>
															{copy.merge.updateFailed} {analysisError}
														</span>
														{analysisUpdateStatus === "paused" && (
															<button
																className="secondary-action"
																type="button"
																onClick={retryAnalysisUpdate}
															>
																{copy.merge.retryUpdate}
															</button>
														)}
													</div>
												)}
												{reviewAvailable && <DispositionRibbon summary={summary} />}
												{!reviewAvailable && (
													<ReviewUnavailablePanel
														state={summary.state}
														inputScope={analysisInputScope}
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
															<p className="section-kicker">{copy.merge.mergeUnits}</p>
															<h2 id="unit-browser-title">
																{pageValue.total.toLocaleString()} {copy.merge.results}
															</h2>
														</div>
														<form className="unit-tools" onSubmit={submitSearch}>
															<label>
																<span className="sr-only">{copy.merge.search}</span>
																<input
																	type="search"
																	placeholder={copy.merge.searchPlaceholder}
																	maxLength={MAX_QUERY_CHARS}
																	value={searchDraft}
																	onChange={(event): void =>
																		setSearchDraft(event.target.value)
																	}
																/>
															</label>
															<button type="submit" className="search-action">
																{copy.merge.search}
															</button>
															<label>
																<span className="sr-only">{copy.merge.filter}</span>
																<select
																	aria-label={copy.merge.filter}
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
																	<option value="all">{copy.merge.allOutcomes}</option>
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
															<p className="panel-placeholder">
																{copy.merge.loadingUnits}
															</p>
														) : unitPage.error !== null ? (
															<p className="error-copy">
																{copy.merge.unitsUnavailable} {unitPage.error}
															</p>
														) : pageValue.items.length === 0 ? (
															<p className="panel-placeholder">{copy.merge.noUnits}</p>
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
																setPage(Math.max(1, displayedPage - 1))
															}
															disabled={unitPage.loading || displayedPage <= 1}
														>
															{copy.merge.previous}
														</button>
														<span>
															{copy.merge.page} {displayedPage} of {pageCount}
														</span>
														<button
															type="button"
															onClick={(): void =>
																setPage(Math.min(pageCount, displayedPage + 1))
															}
															disabled={unitPage.loading || displayedPage >= pageCount}
														>
															{copy.merge.next}
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
										<aside
											className="detail-pane detail-empty"
											aria-label={copy.merge.unitDetail}
										>
											<p className="section-kicker">{copy.merge.unitDetail}</p>
											<h2>{copy.merge.evidenceTitle}</h2>
											<p>{copy.merge.evidenceBody}</p>
										</aside>
									)}
								</div>
							)}
						</>
					)}
				</main>
			</div>
		</CopyContext.Provider>
	);
}
