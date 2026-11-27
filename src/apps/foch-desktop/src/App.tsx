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
	useRef,
	useState,
} from "react";

import {
	MERGE_DISPOSITIONS,
	type AnalysisInputMode,
	type AnalysisInputScope,
	type DesktopClient,
	type DesktopPluginOverview,
	type DesktopPreferences,
	type DesktopResolutionRule,
	type EffectiveLanguage,
	type InputInspection,
	type InputRecoveryOption,
	type MergeAnalysisSummary,
	type MergeAnalysisState,
	type MergeCommitResult,
	type MergeConflictLeaf,
	type MergeDisposition,
	type MergeUnitDetail,
	type MergeUnitListItem,
	type MergeUnitPage,
	type SetupStatus,
	type VerifiedGamePath,
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

function delay(ms: number): Promise<void> {
	return new Promise((resolve): void => {
		setTimeout(resolve, ms);
	});
}

function stageLabel(copy: Copy, stage: string | null): string {
	if (stage === null) return copy.merge.startingCheck;
	if (Object.prototype.hasOwnProperty.call(copy.merge.stages, stage)) {
		return copy.merge.stages[stage as keyof typeof copy.merge.stages];
	}
	return stage;
}

function SetupMeter({
	label,
	completed,
	total,
}: {
	label: string;
	completed: number | null;
	total: number | null;
}): JSX.Element {
	const copy = useCopy();
	const counted: boolean = completed !== null && total !== null;
	return (
		<div className="setup-meter">
			<p>{label}</p>
			{counted ? (
				<>
					<progress value={completed ?? undefined} max={total ?? undefined} />
					<span>
						{completed} / {total}
					</span>
				</>
			) : (
				<p>{copy.merge.stageNoCount}</p>
			)}
		</div>
	);
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
					<span>{copy.merge.installPath}</span>
					<strong title={inspection.game.installPath ?? undefined}>
						{inspection.game.installPath ?? copy.merge.versionMissing}
					</strong>
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
			<p className="recovery-warning">{copy.merge.sessionIgnore}</p>
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

function canExportMerge(state: MergeAnalysisState): boolean {
	return state === "ready" || state === "ready_with_deferrals";
}

function ExportMergedMod({
	analysisId,
	client,
}: {
	analysisId: string;
	client: DesktopClient;
}): JSX.Element {
	const copy = useCopy();
	const [outputDir, setOutputDir] = useState<string>("");
	const [confirming, setConfirming] = useState<boolean>(false);
	const [exporting, setExporting] = useState<boolean>(false);
	const [result, setResult] = useState<MergeCommitResult | null>(null);
	const [error, setError] = useState<string | null>(null);
	const committed: boolean = result !== null;
	const buttonLabel: string = exporting
		? copy.merge.exporting
		: committed
			? copy.merge.committed
			: confirming
				? copy.merge.confirmExport
				: copy.merge.exportAction;

	const exportMod = async (): Promise<void> => {
		if (outputDir.length === 0 || exporting || committed) return;
		if (!confirming) {
			setConfirming(true);
			setError(null);
			return;
		}
		setExporting(true);
		setError(null);
		try {
			setResult(await client.commitMergeAnalysis(analysisId, outputDir));
			setConfirming(false);
		} catch (caught: unknown) {
			setError(parseDesktopCommandError(caught).message);
			setConfirming(false);
		} finally {
			setExporting(false);
		}
	};

	return (
		<section className="export-panel" aria-labelledby="export-merged-mod-title">
			<p className="section-kicker">{copy.merge.exportTitle}</p>
			<h2 id="export-merged-mod-title">{copy.merge.exportTitle}</h2>
			<p>{copy.merge.exportHelp}</p>
			<label className="settings-row">
				<span>{copy.merge.outputFolder}</span>
				<input
					aria-label={copy.merge.outputFolder}
					value={outputDir}
					disabled={exporting || committed}
					onChange={(event): void => {
						setOutputDir(event.target.value);
						setConfirming(false);
					}}
				/>
			</label>
			<button
				className="primary-action"
				type="button"
				disabled={outputDir.length === 0 || exporting || committed}
				onClick={(): void => void exportMod()}
			>
				{buttonLabel}
			</button>
			{result !== null && (
				<div role="status">
					<p className="path-value">{result.outputDir}</p>
					<p>
						{copy.merge.generatedFiles} {result.generatedFileCount}
					</p>
					<p>
						{result.mergeStatus === "partial_success"
							? copy.merge.partialExport
							: copy.merge.readyExport}
					</p>
				</div>
			)}
			{error !== null && (
				<p className="error-copy" role="alert">
					{error}
				</p>
			)}
		</section>
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

function ruleKey(rule: DesktopResolutionRule): string {
	return rule.file ?? rule.matchPattern ?? "";
}

function describeRule(
	copy: Copy,
	rule: DesktopResolutionRule,
	modNames: Map<string, string>,
): string {
	const action = rule.preferMod ?? rule.handler ?? "";
	const scope =
		rule.file !== null ? copy.merge.ruleScopeFile : copy.merge.ruleScopeFolder;
	const target = rule.file ?? rule.matchPattern ?? "";
	const named =
		rule.preferMod !== null ? (modNames.get(rule.preferMod) ?? rule.preferMod) : action;
	return `${scope} · ${target} · ${copy.merge.rulePrefers} ${named}`;
}

function ConflictLeafCard({
	leaf,
	onPrefer,
}: {
	leaf: MergeConflictLeaf;
	onPrefer: (rule: DesktopResolutionRule) => void;
}): JSX.Element {
	const copy = useCopy();
	return (
		<article className="conflict-card" aria-label={leaf.address}>
			<header className="conflict-card-heading">
				<code className="conflict-address">{leaf.address}</code>
				{leaf.kind !== null && <span className="conflict-kind">{leaf.kind}</span>}
			</header>
			{leaf.reason !== null && (
				<p className="conflict-reason">
					<span>{copy.merge.conflictReason}:</span> {leaf.reason}
				</p>
			)}
			{leaf.vanillaSnippet !== null && (
				<div className="conflict-snippet conflict-vanilla">
					<span>{copy.merge.vanillaSnippet}</span>
					<pre>
						<code>{leaf.vanillaSnippet}</code>
					</pre>
				</div>
			)}
			<div className="conflict-candidates" aria-label={copy.merge.competingMods}>
				<p className="conflict-candidates-title">{copy.merge.competingMods}</p>
				{leaf.candidates.map((candidate): JSX.Element => {
					const option = leaf.options.find(
						(option): boolean =>
							option.action.kind === "prefer_mod" &&
							option.action.modId === candidate.modId,
					);
					return (
						<div
							className="candidate-card"
							key={`${candidate.modId}-${candidate.precedence}`}
						>
							<header>
								<strong>{candidate.modDisplayName}</strong>
								<span>#{candidate.precedence + 1}</span>
							</header>
							{candidate.changeSummary.length > 0 && (
								<ul className="candidate-summary">
									{candidate.changeSummary.map((line): JSX.Element => (
										<li key={line}>{line}</li>
									))}
								</ul>
							)}
							<pre className="candidate-rendered">
								<code>{candidate.rendered}</code>
							</pre>
							<div className="candidate-actions">
								<button
									type="button"
									className="secondary-action"
									disabled={option?.fileResolution === undefined}
									onClick={(): void => {
										const rule = option?.fileResolution;
										if (rule !== undefined && rule !== null) onPrefer(rule);
									}}
								>
									{copy.merge.preferForThisFile}
								</button>
								<button
									type="button"
									className="secondary-action"
									disabled={option?.directoryResolution === undefined}
									onClick={(): void => {
										const rule = option?.directoryResolution;
										if (rule !== undefined && rule !== null) onPrefer(rule);
									}}
								>
									{copy.merge.preferForFolder}
								</button>
							</div>
						</div>
					);
				})}
			</div>
		</article>
	);
}

function UnitDetailPanel({
	detail,
	onPrefer,
}: {
	detail: AsyncValue<MergeUnitDetail>;
	onPrefer: (rule: DesktopResolutionRule) => void;
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
			{unit.conflicts.length > 0 && (
				<section className="detail-section conflict-section">
					<h3>{copy.merge.leafConflictsTitle}</h3>
					{unit.conflicts.map((group): JSX.Element => (
						<div className="conflict-group" key={group.filePath}>
							<p className="conflict-group-reason">{group.reason}</p>
							{group.leaves.map((leaf): JSX.Element => (
								<ConflictLeafCard
									key={leaf.conflictId}
									leaf={leaf}
									onPrefer={onPrefer}
								/>
							))}
						</div>
					))}
				</section>
			)}
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

function PendingResolutionsBar({
	rules,
	modNames,
	reanalyzing,
	tomlPreview,
	tomlOpen,
	tomlError,
	onRemove,
	onClear,
	onReanalyze,
	onToggleToml,
}: {
	rules: DesktopResolutionRule[];
	modNames: Map<string, string>;
	reanalyzing: boolean;
	tomlPreview: string | null;
	tomlOpen: boolean;
	tomlError: string | null;
	onRemove: (rule: DesktopResolutionRule) => void;
	onClear: () => void;
	onReanalyze: () => void;
	onToggleToml: () => void;
}): JSX.Element | null {
	const copy = useCopy();
	if (rules.length === 0) return null;
	return (
		<section
			className="pending-resolutions"
			aria-label={copy.merge.pendingResolutionsTitle}
		>
			<header className="pending-resolutions-heading">
				<h3>
					{copy.merge.pendingResolutionsTitle} ({rules.length})
				</h3>
				<button type="button" className="secondary-action" onClick={onClear}>
					{copy.merge.clearRules}
				</button>
			</header>
			<p>{copy.merge.pendingResolutionsBody}</p>
			<ul className="pending-rule-list">
				{rules.map((rule): JSX.Element => (
					<li key={ruleKey(rule)}>
						<span>{describeRule(copy, rule, modNames)}</span>
						<button type="button" onClick={(): void => onRemove(rule)}>
							{copy.merge.removeRule}
						</button>
					</li>
				))}
			</ul>
			<div className="pending-resolutions-actions">
				<button
					type="button"
					className="primary-action"
					onClick={onReanalyze}
					disabled={reanalyzing}
				>
					{reanalyzing ? copy.merge.reanalyzing : copy.merge.reanalyzeWithResolutions}
				</button>
				<button type="button" className="secondary-action" onClick={onToggleToml}>
					{tomlOpen ? copy.merge.hideFochToml : copy.merge.copyFochToml}
				</button>
			</div>
			{tomlOpen && tomlError !== null && <p className="error-copy">{tomlError}</p>}
			{tomlOpen && tomlError === null && tomlPreview !== null && (
				<div className="toml-preview">
					<p>{copy.merge.tomlPreviewNote}</p>
					<pre>
						<code>{tomlPreview}</code>
					</pre>
				</div>
			)}
		</section>
	);
}

function SetupPreparation({
	inspection,
	setup,
	buildLocked,
	onVerify,
	onSave,
	onBuild,
	onCancel,
}: {
	inspection: InputInspection;
	setup: SetupStatus | null;
	buildLocked: boolean;
	onVerify: (path: string) => Promise<VerifiedGamePath>;
	onSave: (path: string) => Promise<void>;
	onBuild: () => void;
	onCancel: () => void;
}): JSX.Element | null {
	const copy = useCopy();
	const [draft, setDraft] = useState<string>("");
	const [verified, setVerified] = useState<VerifiedGamePath | null>(null);
	const [pathError, setPathError] = useState<string | null>(null);
	const [verifying, setVerifying] = useState<boolean>(false);
	const needsPath: boolean =
		inspection.game.installPath === null ||
		inspection.issues.some((issue) => issue.id === "eu4_install_unavailable");
	const canBuild: boolean =
		!needsPath &&
		inspection.game.installPath !== null &&
		inspection.game.version !== null &&
		inspection.baseData.state !== "ready";
	const building: boolean = setup?.phase === "building" || buildLocked;
	const countedStage: string | null = setup?.buildStage ?? setup?.stage ?? null;

	return (
		<div className="setup-preparation">
			{needsPath && (
				<form
					className="path-form"
					onSubmit={(event: FormEvent<HTMLFormElement>): void => {
						event.preventDefault();
						if (verified === null || verified.installPath !== draft) return;
						void onSave(verified.installPath).catch((error: unknown): void => {
							setPathError(errorMessage(error));
						});
					}}
				>
					<label>
						{copy.merge.gamePathLabel}
						<input
							value={draft}
							onChange={(event): void => {
								setDraft(event.target.value);
								setVerified(null);
								setPathError(null);
							}}
						/>
					</label>
					<button
						type="button"
						className="secondary-action"
						disabled={verifying || draft.trim() === ""}
						onClick={(): void => {
							setVerifying(true);
							setPathError(null);
							void onVerify(draft)
								.then((result: VerifiedGamePath): void => {
									setVerified(result);
								})
								.catch((error: unknown): void => {
									setVerified(null);
									setPathError(errorMessage(error));
								})
								.finally((): void => {
									setVerifying(false);
								});
						}}
					>
						{copy.merge.verifyPath}
					</button>
					<button
						type="submit"
						className="primary-action"
						disabled={verified === null || verified.installPath !== draft}
					>
						{copy.merge.useInstall}
					</button>
					{pathError !== null && (
						<p className="error-copy" role="alert">
							{pathError}
						</p>
					)}
					{verified !== null && verified.installPath === draft && (
						<p>
							{copy.merge.verifiedVersion} {verified.version}
						</p>
					)}
				</form>
			)}
			{canBuild && setup?.installNote !== null && setup?.installNote !== undefined && (
				<div>
					<p>{setup.installNote.reason}</p>
					<p>
						{setup.installNote.version} · {setup.installNote.rulesVersion}
					</p>
					<p className="setup-command">
						<code>{setup.installNote.localCommand}</code>
					</p>
				</div>
			)}
			{building && setup !== null && (
				<>
					<SetupMeter
						label={stageLabel(copy, countedStage)}
						completed={
							countedStage === "parse_documents"
								? setup.documentCompleted
								: setup.completed
						}
						total={
							countedStage === "parse_documents" ? setup.documentTotal : setup.total
						}
					/>
					{setup.documentTotal !== null &&
						countedStage !== "parse_documents" &&
						setup.documentCompleted !== null && (
							<p>
								{copy.merge.parsingDocuments} {setup.documentCompleted} /{" "}
								{setup.documentTotal}
							</p>
						)}
					<button
						type="button"
						className="secondary-action"
						onClick={onCancel}
						disabled={setup.buildId === null}
					>
						{copy.merge.cancelBuild}
					</button>
				</>
			)}
			{canBuild && !building && (
				<button
					type="button"
					className="primary-action"
					onClick={onBuild}
					disabled={building}
				>
					{copy.merge.buildBase}
				</button>
			)}
			{setup?.phase === "build_failed" && setup.message !== null && (
				<p className="error-copy" role="alert">
					{setup.message}
				</p>
			)}
			{setup?.phase === "build_cancelled" && setup.message !== null && (
				<p>{setup.message}</p>
			)}
			{setup?.phase === "building" && setup.message !== null && <p>{setup.message}</p>}
		</div>
	);
}

export default function App({
	client = tauriDesktopClient,
	pollIntervalMs = 900,
}: AppProps): JSX.Element {
	const [inspection, setInspection] =
		useState<AsyncValue<InputInspection>>(initialAsyncValue<InputInspection>());
	const [setup, setSetup] = useState<SetupStatus | null>(null);
	const buildLock = useRef<boolean>(false);
	const mounted = useRef(true);
	const [buildLocked, setBuildLocked] = useState<boolean>(false);
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
	const [pendingResolutions, setPendingResolutions] = useState<DesktopResolutionRule[]>(
		[],
	);
	const [reanalyzing, setReanalyzing] = useState<boolean>(false);
	const [tomlOpen, setTomlOpen] = useState<boolean>(false);
	const [tomlPreview, setTomlPreview] = useState<string | null>(null);
	const [tomlError, setTomlError] = useState<string | null>(null);
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

	useEffect((): (() => void) => {
		mounted.current = true;
		return (): void => {
			mounted.current = false;
		};
	}, []);

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

	const applySetup = useCallback((status: SetupStatus): void => {
		setSetup(status);
		if (status.phase === "failed" && status.inspection === null) {
			setInspection({
				value: null,
				loading: false,
				error: status.message ?? "EU4 setup could not be inspected",
			});
			return;
		}
		if (
			status.inspection !== null &&
			(status.phase === "checked" || status.phase === "failed")
		) {
			setInspection({
				value: status.inspection,
				loading: false,
				error: status.phase === "failed" ? status.message : null,
			});
			return;
		}
		if (status.inspection !== null) {
			setInspection({
				value: status.inspection,
				loading: status.phase === "checking",
				error: null,
			});
		}
	}, []);

	const inspectInput = useCallback(async (): Promise<void> => {
		setAnalysisError(null);
		setInspection((current): AsyncValue<InputInspection> => ({
			value: current.value,
			loading: true,
			error: null,
		}));
		try {
			applySetup(await client.startSetupCheck());
		} catch (error: unknown) {
			setInspection((current): AsyncValue<InputInspection> => ({
				value: current.value,
				loading: false,
				error: errorMessage(error),
			}));
		}
	}, [applySetup, client]);

	// Keep the foch.toml preview in step with the queued rules, so the toggle
	// always shows the exact TOML Rust would load.
	useEffect((): (() => void) | undefined => {
		if (pendingResolutions.length === 0) {
			queueMicrotask((): void => {
				setTomlPreview(null);
				setTomlError(null);
			});
			return undefined;
		}
		let disposed = false;
		void client.renderResolutionsToml(pendingResolutions).then(
			(value: string): void => {
				if (!disposed) {
					setTomlPreview(value);
					setTomlError(null);
				}
			},
			(error: unknown): void => {
				if (!disposed) {
					setTomlPreview(null);
					setTomlError(errorMessage(error));
				}
			},
		);
		return (): void => {
			disposed = true;
		};
	}, [client, pendingResolutions]);

	useEffect((): (() => void) => {
		let disposed = false;
		void client.startSetupCheck().then(
			(status: SetupStatus): void => {
				if (!disposed) applySetup(status);
			},
			(error: unknown): void => {
				if (!disposed) {
					setInspection({
						value: null,
						loading: false,
						error: errorMessage(error),
					});
				}
			},
		);
		return (): void => {
			disposed = true;
		};
	}, [applySetup, client]);

	useEffect((): (() => void) | undefined => {
		if (setup?.phase !== "checking" && setup?.phase !== "building") return undefined;
		let disposed = false;
		const timer = setInterval((): void => {
			void client.getSetupStatus().then(
				(status: SetupStatus): void => {
					if (!disposed) applySetup(status);
				},
				(): void => undefined,
			);
		}, pollIntervalMs);
		return (): void => {
			disposed = true;
			clearInterval(timer);
		};
	}, [applySetup, client, pollIntervalMs, setup?.phase]);

	useEffect((): void => {
		if (setup?.phase !== "building") {
			buildLock.current = false;
			setBuildLocked(false);
		}
	}, [setup?.phase]);

	const startBuild = useCallback((): void => {
		if (buildLock.current || setup?.phase === "building") return;
		buildLock.current = true;
		setBuildLocked(true);
		void client.startBaseDataBuild().then(
			(status: SetupStatus): void => {
				applySetup(status);
			},
			(error: unknown): void => {
				buildLock.current = false;
				setBuildLocked(false);
				setAnalysisError(errorMessage(error));
			},
		);
	}, [applySetup, client, setup?.phase]);

	const cancelBuild = useCallback((): void => {
		const buildId: string | null = setup?.buildId ?? null;
		if (buildId === null) return;
		void client.cancelBaseDataBuild(buildId).then(
			(status: SetupStatus): void => {
				applySetup(status);
			},
			(error: unknown): void => {
				setAnalysisError(errorMessage(error));
			},
		);
	}, [applySetup, client, setup?.buildId]);

	const verifyPath = useCallback(
		(path: string): Promise<VerifiedGamePath> => client.verifyGamePath(path),
		[client],
	);

	const savePath = useCallback(
		async (path: string): Promise<void> => {
			await client.saveGamePath(path);
			await inspectInput();
		},
		[client, inspectInput],
	);

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

	const startAnalysis = async (
		inputMode: AnalysisInputMode,
		resolutions: DesktopResolutionRule[] = [],
		freshInspection?: InputInspection,
	): Promise<boolean> => {
		const source = freshInspection ?? inspection.value;
		if (source === null) return false;
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
				source.inspectionId,
				inputMode,
				resolutions,
			);
			setAnalysisInputScope(started.inputScope);
			setAnalysisId(started.analysisId);
			setSummary(queuedAnalysisSummary(started.analysisId));
			return true;
		} catch (error: unknown) {
			setAnalysisError(errorMessage(error));
			return false;
		} finally {
			setStarting(false);
		}
	};

	const queueResolution = (rule: DesktopResolutionRule): void => {
		setPendingResolutions((current): DesktopResolutionRule[] => {
			const key = ruleKey(rule);
			const next = current.filter((existing): boolean => ruleKey(existing) !== key);
			next.push(rule);
			return next;
		});
	};

	const removeResolution = (rule: DesktopResolutionRule): void => {
		setPendingResolutions((current): DesktopResolutionRule[] =>
			current.filter((existing): boolean => ruleKey(existing) !== ruleKey(rule)),
		);
	};

	const reanalyzeWithResolutions = async (): Promise<void> => {
		if (pendingResolutions.length === 0 || reanalyzing) return;
		setReanalyzing(true);
		try {
			// start_check returns while the inspection is still running.
			let status = await client.startSetupCheck();
			if (!mounted.current) return;
			applySetup(status);
			while (status.phase === "checking") {
				await delay(pollIntervalMs);
				if (!mounted.current) return;
				status = await client.getSetupStatus();
				if (!mounted.current) return;
				applySetup(status);
			}
			const fresh = status.inspection;
			if (fresh === null || status.phase !== "checked") return;
			const mode: AnalysisInputMode = analysisInputScope?.mode ?? "complete";
			const started = await startAnalysis(mode, pendingResolutions, fresh);
			if (started && mounted.current) setPendingResolutions([]);
		} catch (error: unknown) {
			if (mounted.current) setAnalysisError(errorMessage(error));
		} finally {
			if (mounted.current) setReanalyzing(false);
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
	const modNames = useMemo((): Map<string, string> => {
		const unit = unitDetail.value;
		if (unit === null) return new Map<string, string>();
		return new Map<string, string>(
			unit.contributors.map((contributor): [string, string] => [
				contributor.modId,
				contributor.name,
			]),
		);
	}, [unitDetail]);
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
									{setup === null ? (
										<p>{copy.merge.startingCheck}</p>
									) : (
										<SetupMeter
											label={stageLabel(copy, setup.stage)}
											completed={setup.completed}
											total={setup.total}
										/>
									)}
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
												<SetupPreparation
													inspection={inspection.value}
													setup={setup}
													buildLocked={buildLocked}
													onVerify={verifyPath}
													onSave={savePath}
													onBuild={startBuild}
													onCancel={cancelBuild}
												/>
												<button
													className="primary-action"
													onClick={(): void => void inspectInput()}
													disabled={inspection.loading || setup?.phase === "building"}
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
												{reviewAvailable && (
													<PendingResolutionsBar
														rules={pendingResolutions}
														modNames={modNames}
														reanalyzing={reanalyzing}
														tomlPreview={tomlPreview}
														tomlOpen={tomlOpen}
														tomlError={tomlError}
														onRemove={removeResolution}
														onClear={(): void => setPendingResolutions([])}
														onReanalyze={(): void => void reanalyzeWithResolutions()}
														onToggleToml={(): void =>
															setTomlOpen((open): boolean => !open)
														}
													/>
												)}
												{canExportMerge(summary.state) && (
													<ExportMergedMod
														key={summary.analysisId}
														analysisId={summary.analysisId}
														client={client}
													/>
												)}
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
											onPrefer={queueResolution}
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
