/**
 * Settings → RAG / WeKnora (headless web host).
 *
 * P1 surface for the WeKnora sync engine: connection + KB picker, what gets
 * rendered into the KB, auto-sync, tool exposure, sync actions and live
 * progress. The `weknora_*` RPCs only exist on the web host; on a desktop
 * build the pane explains that instead of calling them.
 *
 * Backend-not-ready tolerance: a missing/unknown RPC renders an "unavailable"
 * banner and disables the form — never an uncaught error.
 */

import {
	ChevronRight,
	CircleAlert,
	CloudUpload,
	Database,
	LoaderCircle,
	RefreshCw,
	RotateCw,
	ShieldAlert,
	Trash2,
	XCircle,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
	PageTitle,
	SettingsGroup,
	SettingsRow,
	SettingsSectionLabel,
} from "@/components/settings/settings-layout";
import { StatusDot } from "@/components/settings/status-dot";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
	Collapsible,
	CollapsibleContent,
	CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Switch } from "@/components/ui/switch";
import { formatLocaleTimestamp } from "@/i18n";
import {
	errorMessage,
	notifyError,
	notifySuccess,
	notifyWarning,
} from "@/lib/core/notify";
import { isWebHost } from "@/lib/core/tauri";
import { cn } from "@/lib/core/utils";
import type { AppSettings } from "@/lib/settings";
import {
	isWeKnoraUnavailableError,
	listenWeKnoraChanged,
	listenWeKnoraSync,
	weknoraClearMapping,
	weknoraListKbs,
	weknoraReconcile,
	weknoraStatus,
	weknoraSyncCancel,
	weknoraSyncStart,
	weknoraTest,
} from "@/lib/weknora/api";
import {
	hasStoredWeKnoraApiKey,
	normalizeWeKnoraSettings,
	type WeKnoraKnowledgeBase,
	type WeKnoraSettings,
	type WeKnoraStatus,
	type WeKnoraStatusProgress,
	type WeKnoraSyncProgress,
	type WeKnoraSyncSettings,
	type WeKnoraTestResult,
	type WeKnoraToolsSettings,
} from "@/lib/weknora/types";

type ActionKind = "sync" | "cancel" | "reconcile" | "clear" | null;

/** Row helper: a labeled switch with fixed copy. */
function ToggleRow({
	label,
	description,
	checked,
	disabled,
	onCheckedChange,
}: {
	label: string;
	description?: string;
	checked: boolean;
	disabled: boolean;
	onCheckedChange: (checked: boolean) => void;
}) {
	return (
		<SettingsRow label={label} description={description}>
			<Switch
				checked={checked}
				disabled={disabled}
				onCheckedChange={onCheckedChange}
			/>
		</SettingsRow>
	);
}

export function RagPane({
	settings,
	patch,
}: {
	settings: AppSettings;
	patch: (p: Partial<AppSettings>) => void;
}) {
	const { t } = useTranslation(["settings", "common"]);
	const webHost = isWebHost();
	const weknora = useMemo(
		() => normalizeWeKnoraSettings(settings.weknora),
		[settings.weknora],
	);

	const [status, setStatus] = useState<WeKnoraStatus | null>(null);
	const [statusError, setStatusError] = useState<string | null>(null);
	const [loading, setLoading] = useState(true);
	const [testState, setTestState] = useState<"idle" | "testing" | "done">(
		"idle",
	);
	const [testResult, setTestResult] = useState<WeKnoraTestResult | null>(null);
	const [testError, setTestError] = useState<string | null>(null);
	const [kbs, setKbs] = useState<WeKnoraKnowledgeBase[] | null>(null);
	const [kbLoading, setKbLoading] = useState(false);
	const [kbError, setKbError] = useState<string | null>(null);
	const [progress, setProgress] = useState<
		WeKnoraSyncProgress | WeKnoraStatusProgress | null
	>(null);
	const [busy, setBusy] = useState<ActionKind>(null);
	const [errorsOpen, setErrorsOpen] = useState(false);
	const [clearOpen, setClearOpen] = useState(false);
	const [clearDeleteDocs, setClearDeleteDocs] = useState(false);

	const [baseUrlDraft, setBaseUrlDraft] = useState(weknora.baseUrl);
	const [apiKeyDraft, setApiKeyDraft] = useState("");
	const [kbIdDraft, setKbIdDraft] = useState(weknora.knowledgeBaseId);
	const [minutesDraft, setMinutesDraft] = useState(
		String(weknora.sync.autoSyncMinutes),
	);

	const kbsAutoLoadedRef = useRef(false);

	const patchWeKnora = useCallback(
		(update: Partial<WeKnoraSettings>) => {
			patch({ weknora: { ...weknora, ...update } });
		},
		[patch, weknora],
	);
	const updateSync = useCallback(
		(update: Partial<WeKnoraSyncSettings>) => {
			patchWeKnora({ sync: { ...weknora.sync, ...update } });
		},
		[patchWeKnora, weknora.sync],
	);
	const updateTools = useCallback(
		(update: Partial<WeKnoraToolsSettings>) => {
			patchWeKnora({ tools: { ...weknora.tools, ...update } });
		},
		[patchWeKnora, weknora.tools],
	);

	/* ----------------------------- status load ----------------------------- */

	const loadStatus = useCallback(async () => {
		if (!webHost) return;
		try {
			const next = await weknoraStatus();
			setStatus(next);
			setStatusError(null);
			if (next.sync?.progress) setProgress(next.sync.progress);
		} catch (error) {
			setStatus(null);
			setStatusError(errorMessage(error));
		}
	}, [webHost]);

	useEffect(() => {
		if (!webHost) {
			setLoading(false);
			return;
		}
		let cancelled = false;
		setLoading(true);
		void loadStatus().finally(() => {
			if (!cancelled) setLoading(false);
		});
		return () => {
			cancelled = true;
		};
	}, [webHost, loadStatus]);

	/** Live status + sync progress; refresh counters when a run settles. */
	useEffect(() => {
		if (!webHost) return;
		const offs = [
			listenWeKnoraChanged((payload) => {
				setStatus(payload);
				setStatusError(null);
			}),
			listenWeKnoraSync((payload) => {
				setProgress(payload);
				if (!payload.running) void loadStatus();
			}),
		];
		return () => {
			for (const off of offs) off();
		};
	}, [webHost, loadStatus]);

	// Keep text drafts in sync with persistent settings (never the API key).
	useEffect(() => {
		setBaseUrlDraft(weknora.baseUrl);
	}, [weknora.baseUrl]);
	useEffect(() => {
		setKbIdDraft(weknora.knowledgeBaseId);
	}, [weknora.knowledgeBaseId]);
	useEffect(() => {
		setMinutesDraft(String(weknora.sync.autoSyncMinutes));
	}, [weknora.sync.autoSyncMinutes]);

	const ready = webHost && status !== null;
	const configured =
		status?.configured ??
		Boolean(
			weknora.baseUrl.trim() &&
				weknora.apiKey.trim() &&
				weknora.knowledgeBaseId.trim(),
		);
	const apiKeyStored = hasStoredWeKnoraApiKey(weknora.apiKey);
	const running = Boolean(
		status?.sync.running ||
			(progress !== null && "running" in progress && progress.running),
	);
	const disabled = !ready;

	/* ------------------------------ KB loading ----------------------------- */

	const loadKbs = useCallback(async () => {
		if (!webHost) return;
		setKbLoading(true);
		setKbError(null);
		try {
			const result = await weknoraListKbs({
				baseUrl: baseUrlDraft.trim() || undefined,
				apiKey: apiKeyDraft.trim() || undefined,
			});
			const list = Array.isArray(result.kbs) ? result.kbs : [];
			setKbs(list);
			if (result.error) setKbError(result.error);
		} catch (error) {
			setKbs(null);
			setKbError(errorMessage(error));
		} finally {
			setKbLoading(false);
		}
	}, [apiKeyDraft, baseUrlDraft, webHost]);

	// One opportunistic KB list fetch once the backend reports a connection.
	useEffect(() => {
		if (!ready || !configured || kbsAutoLoadedRef.current) return;
		kbsAutoLoadedRef.current = true;
		void loadKbs();
	}, [configured, loadKbs, ready]);

	/* ------------------------------ commits -------------------------------- */

	const commitBaseUrl = useCallback(() => {
		const value = baseUrlDraft.trim().replace(/\/+$/, "");
		setBaseUrlDraft(value);
		setTestState("idle");
		if (value !== weknora.baseUrl) patchWeKnora({ baseUrl: value });
	}, [baseUrlDraft, patchWeKnora, weknora.baseUrl]);

	/** Write-only: an empty input keeps the stored secret untouched. */
	const commitApiKey = useCallback(() => {
		const value = apiKeyDraft.trim();
		setTestState("idle");
		if (!value) return;
		setApiKeyDraft("");
		if (value !== weknora.apiKey) patchWeKnora({ apiKey: value });
	}, [apiKeyDraft, patchWeKnora, weknora.apiKey]);

	const commitKbId = useCallback(() => {
		const value = kbIdDraft.trim();
		setKbIdDraft(value);
		setTestState("idle");
		if (value !== weknora.knowledgeBaseId) {
			patchWeKnora({ knowledgeBaseId: value });
		}
	}, [kbIdDraft, patchWeKnora, weknora.knowledgeBaseId]);

	const commitMinutes = useCallback(() => {
		const parsed = Number.parseInt(minutesDraft, 10);
		const next = Number.isFinite(parsed)
			? Math.min(24 * 60, Math.max(1, parsed))
			: weknora.sync.autoSyncMinutes;
		setMinutesDraft(String(next));
		if (next !== weknora.sync.autoSyncMinutes) {
			updateSync({ autoSyncMinutes: next });
		}
	}, [minutesDraft, updateSync, weknora.sync.autoSyncMinutes]);

	/* ------------------------------ actions -------------------------------- */

	const handleTest = useCallback(async () => {
		if (!webHost) return;
		setTestState("testing");
		setTestResult(null);
		setTestError(null);
		try {
			const result = await weknoraTest({
				baseUrl: baseUrlDraft.trim() || undefined,
				apiKey: apiKeyDraft.trim() || undefined,
				knowledgeBaseId: kbIdDraft.trim() || undefined,
			});
			setTestResult(result);
			setTestState("done");
			if (result.ok) void loadKbs();
		} catch (error) {
			setTestError(errorMessage(error));
			setTestState("done");
		}
	}, [apiKeyDraft, baseUrlDraft, kbIdDraft, loadKbs, webHost]);

	const startSync = useCallback(
		async (args?: { force?: boolean; paper?: string }) => {
			setBusy("sync");
			try {
				const result = await weknoraSyncStart(args ?? {});
				if (!result.started) {
					notifyWarning(t("weknora.ops.alreadyRunning"));
				} else if (!args?.paper) {
					notifySuccess(t("weknora.ops.started", { total: result.total ?? 0 }));
				}
			} catch (error) {
				notifyError(errorMessage(error));
			} finally {
				setBusy(null);
			}
		},
		[t],
	);

	const cancelSync = useCallback(async () => {
		setBusy("cancel");
		try {
			const result = await weknoraSyncCancel();
			if (result.cancelled) notifySuccess(t("weknora.ops.cancelled"));
			else notifyWarning(t("weknora.ops.nothingToCancel"));
		} catch (error) {
			notifyError(errorMessage(error));
		} finally {
			setBusy(null);
		}
	}, [t]);

	const runReconcile = useCallback(async () => {
		setBusy("reconcile");
		try {
			const result = await weknoraReconcile({ prune: false });
			notifySuccess(
				t("weknora.ops.reconcileDone", {
					adopted: result.adopted ?? 0,
					missing: result.missing ?? 0,
					orphans: result.orphans ?? 0,
					deleted: result.deleted ?? 0,
				}),
			);
			await loadStatus();
		} catch (error) {
			notifyError(errorMessage(error));
		} finally {
			setBusy(null);
		}
	}, [loadStatus, t]);

	const runClearMapping = useCallback(async () => {
		setBusy("clear");
		try {
			const result = await weknoraClearMapping({
				deleteDocs: clearDeleteDocs,
			});
			notifySuccess(
				t("weknora.clear.done", {
					cleared: result.cleared ?? 0,
					deleted: result.deleted ?? 0,
				}),
			);
			setClearOpen(false);
			setClearDeleteDocs(false);
			await loadStatus();
		} catch (error) {
			notifyError(errorMessage(error));
		} finally {
			setBusy(null);
		}
	}, [clearDeleteDocs, loadStatus, t]);

	/* ------------------------------- render -------------------------------- */

	if (!webHost) {
		return (
			<section>
				<PageTitle title={t("weknora.title")} />
				<div className="rounded-xl border border-border/70 bg-muted/30 px-3.5 py-3 text-muted-foreground text-xs leading-relaxed">
					{t("weknora.desktopOnly")}
				</div>
			</section>
		);
	}

	const selectedKbName =
		kbs?.find((kb) => kb.id === weknora.knowledgeBaseId)?.name ?? null;
	/** Event payloads carry per-run counters; the persisted status snapshot does not. */
	const progressCounters =
		progress !== null && "created" in progress ? progress : null;
	const syncErrors =
		progress !== null && "errors" in progress ? (progress.errors ?? []) : [];
	const total = status?.sync.total ?? 0;
	const synced = status?.sync.synced ?? 0;
	const indexing = status?.sync.indexing ?? 0;
	const failed = status?.sync.failed ?? 0;
	const pending = status?.sync.pending ?? 0;
	const lastError = status?.sync.lastError?.trim() || null;
	const progressPercent =
		progress && progress.total > 0
			? Math.min(100, Math.round((progress.done / progress.total) * 100))
			: 0;

	const statusLabel = statusError
		? t("weknora.status.backendUnavailable")
		: running
			? t("weknora.status.syncing")
			: status?.enabled && configured
				? t("weknora.status.connected")
				: status?.enabled
					? t("weknora.status.unconfigured")
					: t("weknora.status.disabled");
	const statusTone = statusError
		? "err"
		: running
			? "warn"
			: status?.enabled && configured
				? "ok"
				: "idle";
	const statusText = status
		? `${synced}/${total}${
				status.sync.lastSyncAt
					? ` · ${t("weknora.status.lastSyncAt", {
							time: formatLocaleTimestamp(status.sync.lastSyncAt),
						})}`
					: ""
			}`
		: null;

	const testTone = testResult?.ok
		? "ok"
		: testResult && !testResult.ok
			? "err"
			: testError
				? "err"
				: "idle";
	const testText =
		testState === "testing"
			? t("weknora.test.testing")
			: testError
				? testError
				: testResult?.ok
					? t("weknora.test.okDetail", {
							kbName:
								testResult.kbName?.trim() || weknora.knowledgeBaseId || "—",
							count: testResult.knowledgeCount ?? 0,
							model: testResult.embeddingModelId?.trim() || "—",
						})
					: testResult?.error?.trim()
						? t("weknora.test.failedDetail", { error: testResult.error })
						: t("weknora.test.failed");

	const kbDescription = kbError
		? t("weknora.kb.error", { error: kbError })
		: kbLoading
			? t("weknora.kb.loading")
			: kbs
				? t("weknora.kb.loaded", { count: kbs.length })
				: t("weknora.kb.help");

	return (
		<section>
			<PageTitle
				title={
					<span className="inline-flex min-w-0 items-center gap-2">
						<Database
							className="size-4 shrink-0 text-muted-foreground"
							aria-hidden
						/>
						<span className="truncate">{t("weknora.title")}</span>
						<StatusDot tone={statusTone} label={statusLabel} />
						{statusText ? (
							<span className="truncate font-normal text-muted-foreground text-xs tabular-nums">
								{statusText}
							</span>
						) : null}
					</span>
				}
			/>

			{loading ? (
				<p className="mb-4 flex items-center gap-2 text-muted-foreground text-xs">
					<LoaderCircle className="size-3.5 animate-spin" aria-hidden />
					{t("weknora.loading")}
				</p>
			) : null}

			{statusError ? (
				<div
					role="alert"
					className="mb-4 rounded-xl border border-amber-500/40 bg-amber-500/5 px-3.5 py-3 text-xs leading-relaxed"
				>
					<p className="flex items-center gap-1.5 font-medium text-amber-700 dark:text-amber-400">
						<CircleAlert className="size-3.5" aria-hidden />
						{isWeKnoraUnavailableError(statusError)
							? t("weknora.unavailable.title")
							: t("weknora.unavailable.errorTitle")}
					</p>
					<p className="mt-1 text-muted-foreground">
						{isWeKnoraUnavailableError(statusError)
							? t("weknora.unavailable.body")
							: null}
					</p>
					<p className="mt-1 break-words font-mono text-muted-foreground/80">
						{statusError}
					</p>
					<Button
						type="button"
						variant="outline"
						size="xs"
						className="mt-2"
						onClick={() => void loadStatus()}
					>
						<RefreshCw data-icon="inline-start" className="size-3" />
						{t("weknora.unavailable.retry")}
					</Button>
				</div>
			) : null}

			<SettingsGroup>
				<SettingsRow
					label={t("weknora.enable.label")}
					description={t("weknora.enable.description")}
					htmlFor="weknora-enabled"
				>
					<Switch
						id="weknora-enabled"
						checked={weknora.enabled}
						disabled={disabled}
						onCheckedChange={(enabled) => patchWeKnora({ enabled })}
					/>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("weknora.sections.connection")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("weknora.baseUrl.label")}
					description={t("weknora.baseUrl.help")}
					htmlFor="weknora-base-url"
				>
					<Input
						id="weknora-base-url"
						value={baseUrlDraft}
						placeholder="https://weknora.example.com/api/v1"
						className="h-8 w-full max-w-[24rem] font-mono text-xs"
						spellCheck={false}
						autoComplete="off"
						disabled={disabled}
						onChange={(event) => setBaseUrlDraft(event.currentTarget.value)}
						onBlur={commitBaseUrl}
					/>
				</SettingsRow>
				<SettingsRow
					label={t("weknora.apiKey.label")}
					description={
						apiKeyStored ? t("weknora.apiKey.stored") : t("weknora.apiKey.help")
					}
					htmlFor="weknora-api-key"
				>
					<div className="flex items-center gap-2">
						<Input
							id="weknora-api-key"
							type="password"
							value={apiKeyDraft}
							placeholder={
								apiKeyStored
									? t("weknora.apiKey.placeholderStored")
									: t("weknora.apiKey.placeholder")
							}
							className="h-8 w-full max-w-[18rem] font-mono text-xs"
							autoComplete="off"
							disabled={disabled}
							onChange={(event) => setApiKeyDraft(event.currentTarget.value)}
							onBlur={commitApiKey}
						/>
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={disabled || testState === "testing"}
							onClick={() => void handleTest()}
						>
							{testState === "testing" ? (
								<LoaderCircle
									data-icon="inline-start"
									className="size-3.5 animate-spin"
								/>
							) : (
								<RefreshCw data-icon="inline-start" className="size-3.5" />
							)}
							{t("weknora.test.button")}
						</Button>
						{testState !== "idle" ? (
							<StatusDot tone={testTone} label={testText} />
						) : null}
					</div>
				</SettingsRow>
				{testState !== "idle" && testState !== "testing" ? (
					<SettingsRow label={t("weknora.test.resultLabel")}>
						<span
							className={cn(
								"max-w-[26rem] truncate text-xs",
								testTone === "ok"
									? "text-emerald-600 dark:text-emerald-400"
									: "text-destructive",
							)}
							title={testText}
						>
							{testText}
						</span>
					</SettingsRow>
				) : null}
				<SettingsRow
					label={t("weknora.kb.label")}
					description={kbDescription}
					htmlFor="weknora-kb"
				>
					<div className="flex items-center gap-2">
						<Input
							id="weknora-kb"
							list="weknora-kb-options"
							value={kbIdDraft}
							placeholder={t("weknora.kb.placeholder")}
							className="h-8 w-full max-w-[20rem] font-mono text-xs"
							spellCheck={false}
							autoComplete="off"
							disabled={disabled}
							onChange={(event) => setKbIdDraft(event.currentTarget.value)}
							onBlur={commitKbId}
						/>
						<datalist id="weknora-kb-options">
							{(kbs ?? []).map((kb) => (
								<option key={kb.id} value={kb.id}>
									{`${kb.name} (${kb.knowledgeCount})`}
								</option>
							))}
						</datalist>
						<Button
							type="button"
							variant="ghost"
							size="icon"
							className="size-8"
							aria-label={t("weknora.kb.refresh")}
							disabled={disabled || kbLoading}
							onClick={() => void loadKbs()}
						>
							{kbLoading ? (
								<LoaderCircle className="size-4 animate-spin" />
							) : (
								<RefreshCw className="size-4" />
							)}
						</Button>
					</div>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("weknora.sections.content")}
			</SettingsSectionLabel>
			<SettingsGroup>
				{(
					[
						{
							key: "includePaperBody",
							label: t("weknora.content.paperBody"),
						},
						{
							key: "includeNotes",
							label: t("weknora.content.notes"),
						},
						{
							key: "includeMarks",
							label: t("weknora.content.marks"),
						},
						{
							key: "includeFigureIndex",
							label: t("weknora.content.figureIndex"),
						},
						{
							key: "includeCitation",
							label: t("weknora.content.citation"),
						},
						{
							key: "ensurePaperBody",
							label: t("weknora.content.ensurePaperBody"),
							description: t("weknora.content.ensurePaperBodyHelp"),
						},
					] as const
				).map((item) => (
					<ToggleRow
						key={item.key}
						label={item.label}
						description={"description" in item ? item.description : undefined}
						checked={weknora.sync[item.key]}
						disabled={disabled}
						onCheckedChange={(checked) =>
							updateSync({
								[item.key]: checked,
							} as Partial<WeKnoraSyncSettings>)
						}
					/>
				))}
				<div className="space-y-1.5 border-border/50 border-t px-3.5 py-2.5 text-muted-foreground text-xs leading-relaxed">
					<p className="flex items-start gap-1.5">
						<ShieldAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
						<span>{t("weknora.privacy.pdf")}</span>
					</p>
					<p>{t("weknora.privacy.notes")}</p>
				</div>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("weknora.sections.autoSync")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("weknora.autoSync.label")}
					description={t("weknora.autoSync.description")}
					htmlFor="weknora-auto-sync"
				>
					<Switch
						id="weknora-auto-sync"
						checked={weknora.sync.autoSync}
						disabled={disabled}
						onCheckedChange={(autoSync) => updateSync({ autoSync })}
					/>
				</SettingsRow>
				<SettingsRow
					label={t("weknora.autoSync.interval")}
					htmlFor="weknora-auto-minutes"
				>
					<div className="flex items-center gap-2">
						<Input
							id="weknora-auto-minutes"
							type="number"
							min={1}
							max={24 * 60}
							step={5}
							className="h-8 w-24"
							value={minutesDraft}
							disabled={disabled || !weknora.sync.autoSync}
							onChange={(event) => setMinutesDraft(event.currentTarget.value)}
							onBlur={commitMinutes}
							onKeyDown={(event) => {
								if (event.key !== "Enter") return;
								event.preventDefault();
								event.currentTarget.blur();
							}}
						/>
						<span className="text-muted-foreground text-xs">
							{t("weknora.autoSync.minutesUnit")}
						</span>
					</div>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>{t("weknora.sections.tools")}</SettingsSectionLabel>
			<SettingsGroup>
				<ToggleRow
					label={t("weknora.tools.agent")}
					description={t("weknora.tools.agentHelp")}
					checked={weknora.tools.exposeToAgent}
					disabled={disabled}
					onCheckedChange={(exposeToAgent) => updateTools({ exposeToAgent })}
				/>
				<ToggleRow
					label={t("weknora.tools.mcp")}
					description={t("weknora.tools.mcpHelp")}
					checked={weknora.tools.exposeToMcp}
					disabled={disabled}
					onCheckedChange={(exposeToMcp) => updateTools({ exposeToMcp })}
				/>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("weknora.sections.actions")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("weknora.ops.syncLabel")}
					description={
						!weknora.enabled
							? t("weknora.ops.needEnabled")
							: !configured
								? t("weknora.ops.needConfigured")
								: t("weknora.ops.syncDetail", { total })
					}
				>
					<div className="flex items-center gap-2">
						{running ? (
							<Button
								type="button"
								variant="outline"
								size="sm"
								disabled={busy !== null || !ready}
								onClick={() => void cancelSync()}
							>
								{busy === "cancel" ? (
									<LoaderCircle
										data-icon="inline-start"
										className="size-3.5 animate-spin"
									/>
								) : (
									<XCircle data-icon="inline-start" className="size-3.5" />
								)}
								{t("weknora.ops.cancel")}
							</Button>
						) : null}
						<Button
							type="button"
							size="sm"
							disabled={
								busy !== null ||
								running ||
								!ready ||
								!weknora.enabled ||
								!configured
							}
							onClick={() => void startSync()}
						>
							{busy === "sync" ? (
								<LoaderCircle
									data-icon="inline-start"
									className="size-3.5 animate-spin"
								/>
							) : (
								<CloudUpload data-icon="inline-start" className="size-3.5" />
							)}
							{t("weknora.ops.syncNow")}
						</Button>
					</div>
				</SettingsRow>
				<SettingsRow
					label={t("weknora.ops.reconcileLabel")}
					description={t("weknora.ops.reconcileDescription")}
				>
					<Button
						type="button"
						variant="outline"
						size="sm"
						disabled={busy !== null || running || !ready || !configured}
						onClick={() => void runReconcile()}
					>
						{busy === "reconcile" ? (
							<LoaderCircle
								data-icon="inline-start"
								className="size-3.5 animate-spin"
							/>
						) : (
							<RotateCw data-icon="inline-start" className="size-3.5" />
						)}
						{t("weknora.ops.reconcile")}
					</Button>
				</SettingsRow>
				<SettingsRow
					label={
						<span className="text-destructive">
							{t("weknora.ops.clearLabel")}
						</span>
					}
					description={t("weknora.ops.clearDescription")}
				>
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy !== null || running || !ready}
						onClick={() => setClearOpen(true)}
					>
						<Trash2 data-icon="inline-start" className="size-3.5" />
						{t("weknora.ops.clear")}
					</Button>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("weknora.sections.status")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow label={t("weknora.status.label")}>
					<span className="flex flex-wrap items-center justify-end gap-x-3 gap-y-1 text-muted-foreground text-xs tabular-nums">
						<span className="font-medium text-foreground">
							{t("weknora.status.synced", { synced, total })}
						</span>
						{indexing > 0 ? (
							<span>{t("weknora.status.indexing", { count: indexing })}</span>
						) : null}
						{pending > 0 ? (
							<span>{t("weknora.status.pending", { count: pending })}</span>
						) : null}
						{failed > 0 ? (
							<span className="text-destructive">
								{t("weknora.status.failed", { count: failed })}
							</span>
						) : null}
					</span>
				</SettingsRow>
				<SettingsRow label={t("weknora.status.lastSync")}>
					<span className="text-muted-foreground text-xs tabular-nums">
						{status?.sync.lastSyncAt
							? formatLocaleTimestamp(status.sync.lastSyncAt)
							: t("weknora.status.neverSynced")}
					</span>
				</SettingsRow>
				{status?.kbName || selectedKbName ? (
					<SettingsRow label={t("weknora.status.kb")}>
						<span className="max-w-[24rem] truncate text-muted-foreground text-xs">
							{status?.kbName ?? selectedKbName}
						</span>
					</SettingsRow>
				) : null}
				{lastError ? (
					<div
						role="alert"
						className="border-border/50 border-b px-3.5 py-2.5 text-destructive text-xs leading-relaxed last:border-b-0"
					>
						{t("weknora.status.lastError", { error: lastError })}
					</div>
				) : null}
				{progress ? (
					<div className="border-border/50 border-b px-3.5 py-2.5 last:border-b-0">
						<div className="flex items-center justify-between gap-3 text-xs">
							<span className="font-medium">
								{running
									? t(`weknora.sync.phase.${progress.phase}`)
									: `${t("weknora.sync.lastRun")} · ${t(
											`weknora.sync.phase.${progress.phase}`,
										)}`}
							</span>
							<span className="text-muted-foreground tabular-nums">
								{progress.done}/{progress.total}
							</span>
						</div>
						<Progress value={progressPercent} className="mt-2 h-1.5" />
						{progress.current ? (
							<p
								className="mt-1.5 truncate text-muted-foreground text-xs"
								title={progress.current}
							>
								{progress.current}
							</p>
						) : null}
						{progressCounters ? (
							<div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground text-xs tabular-nums">
								<span>
									{t("weknora.sync.counters.created", {
										count: progressCounters.created,
									})}
								</span>
								<span>
									{t("weknora.sync.counters.updated", {
										count: progressCounters.updated,
									})}
								</span>
								<span>
									{t("weknora.sync.counters.skipped", {
										count: progressCounters.skipped,
									})}
								</span>
								<span>
									{t("weknora.sync.counters.deleted", {
										count: progressCounters.deleted,
									})}
								</span>
								<span
									className={cn(
										progressCounters.failed > 0 && "text-destructive",
									)}
								>
									{t("weknora.sync.counters.failed", {
										count: progressCounters.failed,
									})}
								</span>
							</div>
						) : null}
					</div>
				) : null}
				{syncErrors.length > 0 ? (
					<Collapsible
						open={errorsOpen}
						onOpenChange={setErrorsOpen}
						className="border-border/50 border-b last:border-b-0"
					>
						<CollapsibleTrigger asChild>
							<button
								type="button"
								className="flex w-full items-center gap-1.5 px-3.5 py-2.5 text-left text-muted-foreground text-xs outline-none transition-colors hover:text-foreground"
							>
								<ChevronRight
									className={cn(
										"size-3.5 shrink-0 transition-transform",
										errorsOpen && "rotate-90",
									)}
									aria-hidden
								/>
								{t("weknora.sync.errorsTitle", {
									count: syncErrors.length,
								})}
							</button>
						</CollapsibleTrigger>
						<CollapsibleContent>
							<ul className="space-y-1 px-3.5 pb-2.5">
								{syncErrors.slice(0, 100).map((item) => (
									<li
										key={`${item.paper}\u0000${item.error}`}
										className="flex items-start justify-between gap-2 rounded-md border border-destructive/20 bg-destructive/5 px-2 py-1.5 text-xs"
									>
										<div className="min-w-0">
											<p
												className="truncate font-medium text-foreground"
												title={item.paper}
											>
												{item.paper}
											</p>
											<p className="break-words text-muted-foreground">
												{item.error}
											</p>
										</div>
										<Button
											type="button"
											variant="ghost"
											size="xs"
											disabled={busy !== null || running}
											onClick={() =>
												void startSync({ force: true, paper: item.paper })
											}
										>
											{t("weknora.sync.retry")}
										</Button>
									</li>
								))}
							</ul>
						</CollapsibleContent>
					</Collapsible>
				) : null}
			</SettingsGroup>

			<Dialog
				open={clearOpen}
				onOpenChange={(open) => {
					if (!open && busy !== "clear") setClearOpen(false);
				}}
			>
				<DialogContent
					showCloseButton={false}
					className="sm:max-w-sm"
					aria-describedby={undefined}
				>
					<DialogHeader>
						<DialogTitle>{t("weknora.clear.title")}</DialogTitle>
					</DialogHeader>
					<div className="space-y-2 py-1 text-muted-foreground text-xs leading-relaxed">
						<p>{t("weknora.clear.body")}</p>
						<label
							htmlFor="weknora-clear-delete-docs"
							className="flex items-start gap-2 text-foreground"
						>
							<Checkbox
								id="weknora-clear-delete-docs"
								checked={clearDeleteDocs}
								disabled={busy === "clear"}
								onCheckedChange={(checked) =>
									setClearDeleteDocs(checked === true)
								}
							/>
							<span>
								{t("weknora.clear.deleteDocs")}
								<span className="mt-0.5 block text-muted-foreground text-xs">
									{t("weknora.clear.deleteDocsHint")}
								</span>
							</span>
						</label>
					</div>
					<DialogFooter className="gap-2 sm:gap-0">
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={busy === "clear"}
							onClick={() => setClearOpen(false)}
						>
							{t("common:cancel")}
						</Button>
						<Button
							type="button"
							variant="destructive"
							size="sm"
							disabled={busy === "clear"}
							onClick={() => void runClearMapping()}
						>
							{busy === "clear" ? (
								<LoaderCircle className="mr-1.5 size-3.5 animate-spin" />
							) : null}
							{t("weknora.clear.confirm")}
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</section>
	);
}
