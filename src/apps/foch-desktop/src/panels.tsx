import { type FormEvent, type JSX, useState } from "react";

import type {
	DesktopClient,
	DesktopLaunchResult,
	DesktopLaunchStatus,
	DesktopPluginEntry,
	DesktopPluginOverview,
	DesktopPreferences,
	EffectiveLanguage,
} from "./api";
import { parseDesktopCommandError } from "./api";
import type { Copy } from "./i18n";

interface AsyncValue<T> {
	value: T | null;
	loading: boolean;
	error: string | null;
}

function message(error: unknown): string {
	return parseDesktopCommandError(error).message;
}

function supportedGame(version: string | null): boolean {
	if (version === null) return false;
	const trimmed = version.trim().replace(/^[vV]/, "");
	const [major, minor, patch = "0"] = trimmed.split(".");
	return major === "1" && minor === "37" && patch.match(/^\d+/)?.[0] === "5";
}

function runtimeLabel(copy: Copy, state: string): string {
	switch (state) {
		case "active":
			return copy.play.active;
		case "refused":
			return copy.play.refused;
		case "failed":
			return copy.play.failed;
		case "not_loaded":
			return copy.play.notLoaded;
		default:
			return state;
	}
}

function badgeLabel(copy: Copy, badge: DesktopPluginEntry["badge"]): string {
	switch (badge) {
		case "enabled":
			return copy.plugins.enabled;
		case "installed":
			return copy.plugins.installed;
		case "conflict":
			return copy.plugins.conflict;
		default:
			return copy.plugins.missing;
	}
}

export function PlayPanel({
	client,
	playset,
	overview,
	copy,
	onReload,
}: {
	client: DesktopClient;
	playset: string | null;
	overview: AsyncValue<DesktopPluginOverview>;
	copy: Copy;
	onReload: () => void;
}): JSX.Element {
	const [confirming, setConfirming] = useState(false);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [launch, setLaunch] = useState<DesktopLaunchResult | null>(null);
	const [status, setStatus] = useState<DesktopLaunchStatus | null>(null);
	const view = overview.value;
	const launchable = view?.plan?.launchable === true && view.hostDllAvailable;
	const missing = view?.entries.filter(
		(entry): boolean =>
			entry.declaredByMods.length > 0 &&
			entry.installedVersions.length === 0 &&
			entry.installSpec !== null,
	);

	async function run(prepareOnly: boolean): Promise<void> {
		setBusy(true);
		setError(null);
		try {
			const result = await client.launchEu4WithPlugins(playset, prepareOnly);
			setLaunch(result);
			setStatus(await client.getLaunchStatus(result.runDir));
		} catch (caught: unknown) {
			setError(message(caught));
		} finally {
			setBusy(false);
			setConfirming(false);
		}
	}

	async function install(entry: DesktopPluginEntry): Promise<void> {
		if (entry.installSpec === null) return;
		setBusy(true);
		setError(null);
		try {
			await client.installPlugin(entry.installSpec, null);
			onReload();
		} catch (caught: unknown) {
			setError(message(caught));
		} finally {
			setBusy(false);
		}
	}

	return (
		<section className="play-panel" aria-label={copy.play.title}>
			<p className="section-kicker">{copy.play.kicker}</p>
			<h2>{copy.play.title}</h2>
			{overview.loading && view === null ? (
				<p>{copy.play.loading}</p>
			) : overview.error !== null && view === null ? (
				<p className="error-copy" role="alert">
					{overview.error}
				</p>
			) : view === null ? null : (
				<>
					<div className="readiness-cards">
						<article className="readiness-card">
							<span>{copy.play.game}</span>
							<strong>{view.gameVersion ?? copy.play.notDetected}</strong>
							<small>
								{supportedGame(view.gameVersion)
									? copy.play.versionReady
									: copy.play.versionDifferent}
							</small>
						</article>
						<article className="readiness-card">
							<span>{copy.play.host}</span>
							<strong>
								{view.hostDllAvailable ? copy.play.hostReady : copy.play.hostMissing}
							</strong>
							<small>{view.hostDllDetail}</small>
						</article>
					</div>
					{view.companionWarnings.length > 0 && (
						<div className="companion-banner" role="status">
							<strong>{copy.play.companionTitle}</strong>
							{view.companionWarnings.map((warning): JSX.Element => (
								<p key={warning}>{warning}</p>
							))}
							<div className="banner-actions">
								{missing?.map((entry): JSX.Element => (
									<button
										key={entry.id}
										type="button"
										className="secondary-action"
										disabled={busy}
										onClick={(): void => void install(entry)}
									>
										{copy.play.installDeclared} {entry.name}
									</button>
								))}
							</div>
						</div>
					)}
					<section className="order-panel" aria-label={copy.play.order}>
						<h3>{copy.play.order}</h3>
						{view.plan === null || view.plan.order.length === 0 ? (
							<p>{copy.play.noOrder}</p>
						) : (
							<ol>
								{view.plan.order.map((item): JSX.Element => {
									const id = item.split(" ")[0] ?? item;
									const entry = view.entries.find((candidate) => candidate.id === id);
									return (
										<li key={item}>
											<span>{item}</span>
											{entry?.badge === "conflict" && (
												<em className="conflict-badge">{copy.play.conflicts}</em>
											)}
										</li>
									);
								})}
							</ol>
						)}
						{view.plan !== null && view.plan.errors.length > 0 && (
							<ul className="plan-errors">
								{view.plan.errors.map((item): JSX.Element => (
									<li key={item}>{item}</li>
								))}
							</ul>
						)}
					</section>
					<div className="launch-actions">
						<button
							type="button"
							className="primary-action"
							disabled={!launchable || busy}
							onClick={(): void => setConfirming(true)}
						>
							{copy.play.launch}
						</button>
						<button
							type="button"
							className="secondary-action"
							disabled={!launchable || busy}
							onClick={(): void => void run(true)}
						>
							{copy.play.prepare}
						</button>
					</div>
					{!launchable && <p>{copy.play.planBlocked}</p>}
					{confirming && (
						<div className="modal-backdrop">
							<div
								className="modal-card"
								role="dialog"
								aria-modal="true"
								aria-labelledby="launch-confirm-title"
							>
								<h2 id="launch-confirm-title">{copy.play.confirmTitle}</h2>
								<p>{copy.play.confirmBody}</p>
								<div className="launch-actions">
									<button
										type="button"
										className="primary-action"
										disabled={busy}
										onClick={(): void => void run(false)}
									>
										{copy.play.confirmLaunch}
									</button>
									<button
										type="button"
										className="secondary-action"
										onClick={(): void => setConfirming(false)}
									>
										{copy.play.cancel}
									</button>
								</div>
							</div>
						</div>
					)}
					<section className="status-panel" aria-label={copy.play.status}>
						<h3>{copy.play.status}</h3>
						{launch === null ? (
							<p>{copy.play.noStatus}</p>
						) : (
							<>
								<p>
									{launch.preparedOnly ? copy.play.prepared : copy.play.started}
									{launch.pid !== null && ` · PID ${launch.pid.toString()}`}
								</p>
								<p className="path-value">{launch.runDir}</p>
								<ul>
									{status?.plugins.map((plugin): JSX.Element => (
										<li key={plugin.id}>
											<strong>{plugin.id}</strong>
											<span>{runtimeLabel(copy, plugin.state)}</span>
											{plugin.reason !== null && <small>{plugin.reason}</small>}
										</li>
									))}
								</ul>
							</>
						)}
					</section>
					{error !== null && (
						<p className="error-copy" role="alert">
							{error}
						</p>
					)}
				</>
			)}
		</section>
	);
}

export function PluginsPanel({
	client,
	playset,
	overview,
	copy,
	onReload,
}: {
	client: DesktopClient;
	playset: string | null;
	overview: AsyncValue<DesktopPluginOverview>;
	copy: Copy;
	onReload: () => void;
}): JSX.Element {
	const [spec, setSpec] = useState("");
	const [sha256, setSha256] = useState("");
	const [directory, setDirectory] = useState("");
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const view = overview.value;

	async function install(event: FormEvent<HTMLFormElement>): Promise<void> {
		event.preventDefault();
		const trimmed = spec.trim();
		if (trimmed === "") return;
		setBusy(true);
		setError(null);
		try {
			await client.installPlugin(trimmed, sha256.trim() === "" ? null : sha256.trim());
			setSpec("");
			setSha256("");
			onReload();
		} catch (caught: unknown) {
			setError(message(caught));
		} finally {
			setBusy(false);
		}
	}

	async function importDirectory(event: FormEvent<HTMLFormElement>): Promise<void> {
		event.preventDefault();
		if (directory.trim() === "") return;
		setBusy(true);
		setError(null);
		try {
			await client.importPluginDir(directory.trim());
			setDirectory("");
			onReload();
		} catch (caught: unknown) {
			setError(message(caught));
		} finally {
			setBusy(false);
		}
	}

	async function toggle(entry: DesktopPluginEntry): Promise<void> {
		setBusy(true);
		setError(null);
		try {
			await client.setPluginEnabled(
				playset,
				entry.id,
				!entry.enabled,
				entry.selectedVersion ??
					entry.installedVersions[entry.installedVersions.length - 1] ??
					null,
			);
			onReload();
		} catch (caught: unknown) {
			setError(message(caught));
		} finally {
			setBusy(false);
		}
	}

	async function saveConfig(
		entry: DesktopPluginEntry,
		key: string,
		value: boolean | number | string,
	): Promise<void> {
		setError(null);
		try {
			await client.updatePluginConfig(playset, entry.id, key, value);
			onReload();
		} catch (caught: unknown) {
			setError(message(caught));
		}
	}

	return (
		<section className="plugins-panel" aria-label={copy.plugins.title}>
			<p className="section-kicker">{copy.plugins.kicker}</p>
			<h2>{copy.plugins.title}</h2>
			<form className="install-form" onSubmit={(event): void => void install(event)}>
				<label>
					<span className="sr-only">{copy.plugins.installSpec}</span>
					<input
						value={spec}
						placeholder={copy.plugins.installSpec}
						aria-label={copy.plugins.installSpec}
						onChange={(event): void => setSpec(event.target.value)}
					/>
				</label>
				<label>
					<span className="sr-only">{copy.plugins.sha256}</span>
					<input
						value={sha256}
						placeholder={copy.plugins.sha256}
						aria-label={copy.plugins.sha256}
						onChange={(event): void => setSha256(event.target.value)}
					/>
				</label>
				<button type="submit" className="primary-action" disabled={busy}>
					{copy.plugins.install}
				</button>
			</form>
			<form
				className="install-form"
				onSubmit={(event): void => void importDirectory(event)}
			>
				<label>
					<span className="sr-only">{copy.plugins.importPath}</span>
					<input
						value={directory}
						placeholder={copy.plugins.importPath}
						aria-label={copy.plugins.importPath}
						onChange={(event): void => setDirectory(event.target.value)}
					/>
				</label>
				<button type="submit" className="secondary-action" disabled={busy}>
					{copy.plugins.import}
				</button>
			</form>
			{overview.loading && view === null ? (
				<p>{copy.plugins.loading}</p>
			) : view === null || view.entries.length === 0 ? (
				<p>{copy.plugins.empty}</p>
			) : (
				<ul className="plugin-list">
					{view.entries.map((entry): JSX.Element => (
						<li key={entry.id} className="plugin-card">
							<div className="plugin-card-heading">
								<div>
									<strong>{entry.name}</strong>
									<small>{entry.id}</small>
								</div>
								<button
									type="button"
									role="switch"
									aria-checked={entry.enabled}
									aria-label={`${copy.plugins.toggle} ${entry.name}`}
									disabled={busy || entry.installedVersions.length === 0}
									onClick={(): void => void toggle(entry)}
								>
									{badgeLabel(copy, entry.badge)}
								</button>
							</div>
							{entry.description !== null && <p>{entry.description}</p>}
							{entry.declaredByMods.length > 0 && (
								<p>
									{copy.plugins.declaredBy}: {entry.declaredByMods.join(", ")}
								</p>
							)}
							{entry.conflicts.length > 0 && (
								<p className="conflict-copy">{entry.conflicts.join(" · ")}</p>
							)}
							{entry.configFields.length > 0 && (
								<fieldset>
									<legend>{copy.plugins.config}</legend>
									{entry.configFields.map((field): JSX.Element => {
										if (field.kind === "bool") {
											return (
												<label key={field.key} className="config-row">
													<input
														type="checkbox"
														checked={field.value === true}
														onChange={(event): void =>
															void saveConfig(entry, field.key, event.target.checked)
														}
													/>
													{field.key}
												</label>
											);
										}
										if (
											field.kind === "int" &&
											field.min !== null &&
											field.max !== null
										) {
											const current =
												typeof field.value === "number" ? field.value : field.min;
											return (
												<label key={field.key} className="config-row">
													<span>
														{field.key} <strong>{current.toString()}</strong>
													</span>
													<input
														type="range"
														min={field.min}
														max={field.max}
														value={current}
														aria-label={field.key}
														onChange={(event): void =>
															void saveConfig(
																entry,
																field.key,
																Number(event.target.value),
															)
														}
													/>
												</label>
											);
										}
										return (
											<label key={field.key} className="config-row">
												<span>{field.key}</span>
												<input
													type={field.kind === "int" ? "number" : "text"}
													aria-label={field.key}
													value={field.value === null ? "" : String(field.value)}
													onChange={(event): void => {
														const next =
															field.kind === "int"
																? Number(event.target.value)
																: event.target.value;
														void saveConfig(entry, field.key, next);
													}}
												/>
											</label>
										);
									})}
								</fieldset>
							)}
						</li>
					))}
				</ul>
			)}
			{error !== null && (
				<p className="error-copy" role="alert">
					{error}
				</p>
			)}
		</section>
	);
}

export function SettingsPanel({
	client,
	preferences,
	copy,
	onPreferences,
	onLanguage,
}: {
	client: DesktopClient;
	preferences: DesktopPreferences | null;
	copy: Copy;
	onPreferences: (value: DesktopPreferences) => void;
	onLanguage: (language: EffectiveLanguage) => void;
}): JSX.Element {
	const [error, setError] = useState<string | null>(null);
	const [saved, setSaved] = useState(false);
	const [mirrorDraft, setMirrorDraft] = useState<string | null>(null);
	const mirrorText = mirrorDraft ?? preferences?.customMirrors.join(", ") ?? "";

	async function commit(next: DesktopPreferences): Promise<void> {
		onPreferences(next);
		if (next.language === "zh_cn") onLanguage("zh-CN");
		else if (next.language === "en") onLanguage("en");
		setError(null);
		setSaved(false);
		try {
			const stored = await client.saveDesktopSettings({
				transport: next.transport,
				mirror: next.mirror,
				customMirrors: next.customMirrors,
				language: next.language,
			});
			onPreferences(stored);
			onLanguage(stored.effectiveLanguage);
			setMirrorDraft(null);
			setSaved(true);
		} catch (caught: unknown) {
			setError(message(caught));
		}
	}

	if (preferences === null) {
		return (
			<section className="settings-panel" aria-label={copy.settings.title}>
				<h2>{copy.settings.title}</h2>
				<p>{copy.settings.loading}</p>
			</section>
		);
	}

	return (
		<section className="settings-panel" aria-label={copy.settings.title}>
			<p className="section-kicker">{copy.settings.kicker}</p>
			<h2>{copy.settings.title}</h2>
			<label className="settings-row">
				<span>{copy.settings.transport}</span>
				<select
					aria-label={copy.settings.transport}
					value={preferences.transport}
					onChange={(event): void =>
						void commit({
							...preferences,
							transport: event.target.value as DesktopPreferences["transport"],
						})
					}
				>
					<option value="auto">{copy.settings.transportAuto}</option>
					<option value="http">{copy.settings.transportHttp}</option>
					<option value="gh">{copy.settings.transportGh}</option>
				</select>
			</label>
			<label className="settings-row">
				<span>{copy.settings.mirror}</span>
				<select
					aria-label={copy.settings.mirror}
					value={preferences.mirror}
					onChange={(event): void => {
						const mirror = event.target.value as DesktopPreferences["mirror"];
						if (mirror === "custom" && preferences.customMirrors.length === 0) {
							onPreferences({ ...preferences, mirror });
							return;
						}
						void commit({ ...preferences, mirror });
					}}
				>
					<option value="auto">{copy.settings.mirrorAuto}</option>
					<option value="cn">{copy.settings.mirrorChina}</option>
					<option value="direct">{copy.settings.mirrorDirect}</option>
					<option value="custom">{copy.settings.mirrorCustom}</option>
				</select>
			</label>
			{preferences.mirror === "custom" && (
				<label className="settings-row">
					<span>{copy.settings.customMirrors}</span>
					<input
						aria-label={copy.settings.customMirrors}
						value={mirrorText}
						placeholder={copy.settings.customHint}
						onChange={(event): void => setMirrorDraft(event.target.value)}
						onBlur={(): void => {
							const customMirrors = mirrorText
								.split(",")
								.map((item): string => item.trim())
								.filter((item): boolean => item !== "");
							if (customMirrors.length === 0) return;
							void commit({ ...preferences, mirror: "custom", customMirrors });
						}}
					/>
				</label>
			)}
			<label className="settings-row">
				<span>{copy.settings.language}</span>
				<select
					aria-label={copy.settings.language}
					value={preferences.language}
					onChange={(event): void =>
						void commit({
							...preferences,
							language: event.target.value as DesktopPreferences["language"],
						})
					}
				>
					<option value="auto">{copy.settings.languageAuto}</option>
					<option value="zh_cn">简体中文</option>
					<option value="en">English</option>
				</select>
			</label>
			{saved && <p role="status">{copy.settings.saved}</p>}
			{error !== null && (
				<p className="error-copy" role="alert">
					{error}
				</p>
			)}
		</section>
	);
}
