/**
 * Settings → RAG / External knowledge base (headless web host).
 *
 * Provider-agnostic surface for the RAG sync engine: provider + connection +
 * KB picker, what gets rendered into the KB, auto-sync, tool exposure, sync
 * actions and live progress. WeKnora is the supported provider in v2.1;
 * RAGFlow is reserved. The `rag_*` RPCs only exist on the web host; on a
 * desktop build the pane explains that instead of calling them.
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
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
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
import {
	isRagUnavailableError,
	listenRagChanged,
	listenRagSync,
	ragClearMapping,
	ragListKbs,
	ragReconcile,
	ragStatus,
	ragSyncCancel,
	ragSyncStart,
	ragTest,
} from "@/lib/rag/api";
import {
	hasStoredRagApiKey,
	isRagProviderId,
	normalizeRagSettings,
	providerLabel,
	RAG_PROVIDER_CHOICES,
	type RagKnowledgeBase,
	type RagSettings,
	type RagStatus,
	type RagStatusProgress,
	type RagSyncProgress,
	type RagSyncSettings,
	type RagTestResult,
	type RagToolsSettings,
} from "@/lib/rag/types";
import type { AppSettings } from "@/lib/settings";

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

/** Full-width field row: label/description on top, wide control row below. */
function WideFieldRow({
	label,
	description,
	htmlFor,
	children,
}: {
	label: string;
	description?: string;
	htmlFor: string;
	children: React.ReactNode;
}) {
	return (
		<div className="border-border/50 border-b px-3.5 py-2.5 last:border-b-0">
			<div className="min-w-0">
				<Label
					htmlFor={htmlFor}
					className="font-normal text-foreground text-sm leading-snug"
				>
					{label}
				</Label>
				{description ? (
					<p className="mt-0.5 text-muted-foreground text-xs leading-relaxed">
						{description}
					</p>
				) : null}
			</div>
			<div className="mt-1.5 flex w-full min-w-0 items-center gap-2">
				{children}
			</div>
		</div>
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
	const rag = useMemo(() => normalizeRagSettings(settings.rag), [settings.rag]);

	const [status, setStatus] = useState<RagStatus | null>(null);
	const [statusError, setStatusError] = useState<string | null>(null);
	const [loading, setLoading] = useState(true);
	const [testState, setTestState] = useState<"idle" | "testing" | "done">(
		"idle",
	);
	const [testResult, setTestResult] = useState<RagTestResult | null>(null);
	const [testError, setTestError] = useState<string | null>(null);
	const [kbs, setKbs] = useState<RagKnowledgeBase[] | null>(null);
	const [kbLoading, setKbLoading] = useState(false);
	const [kbError, setKbError] = useState<string | null>(null);
	const [progress, setProgress] = useState<
		RagSyncProgress | RagStatusProgress | null
	>(null);
	const [busy, setBusy] = useState<ActionKind>(null);
	const [errorsOpen, setErrorsOpen] = useState(false);
	const [clearOpen, setClearOpen] = useState(false);
	const [clearDeleteDocs, setClearDeleteDocs] = useState(false);

	const [baseUrlDraft, setBaseUrlDraft] = useState(rag.baseUrl);
	const [apiKeyDraft, setApiKeyDraft] = useState("");
	const [kbIdDraft, setKbIdDraft] = useState(rag.knowledgeBaseId);
	const [minutesDraft, setMinutesDraft] = useState(
		String(rag.sync.autoSyncMinutes),
	);

	const kbsAutoLoadedRef = useRef(false);

	const patchRag = useCallback(
		(update: Partial<RagSettings>) => {
			patch({ rag: { ...rag, ...update } });
		},
		[patch, rag],
	);
	const updateSync = useCallback(
		(update: Partial<RagSyncSettings>) => {
			patchRag({ sync: { ...rag.sync, ...update } });
		},
		[patchRag, rag.sync],
	);
	const updateTools = useCallback(
		(update: Partial<RagToolsSettings>) => {
			patchRag({ tools: { ...rag.tools, ...update } });
		},
		[patchRag, rag.tools],
	);

	/* ----------------------------- status load ----------------------------- */

	const loadStatus = useCallback(async () => {
		if (!webHost) return;
		try {
			const next = await ragStatus();
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
			listenRagChanged((payload) => {
				setStatus(payload);
				setStatusError(null);
			}),
			listenRagSync((payload) => {
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
		setBaseUrlDraft(rag.baseUrl);
	}, [rag.baseUrl]);
	useEffect(() => {
		setKbIdDraft(rag.knowledgeBaseId);
	}, [rag.knowledgeBaseId]);
	useEffect(() => {
		setMinutesDraft(String(rag.sync.autoSyncMinutes));
	}, [rag.sync.autoSyncMinutes]);

	const statusProviderId =
		typeof status?.provider === "string" && status.provider.trim()
			? status.provider.trim()
			: null;
	const currentProviderLabel =
		status?.providerLabel?.trim() ||
		providerLabel(statusProviderId ?? rag.provider);
	const ready = webHost && status !== null;
	/**
	 * Reserved providers (RAGFlow in v2.1) can be stored in settings but have no
	 * backend implementation yet: keep the provider picker usable so the user can
	 * switch back, but disable everything that would call the provider.
	 */
	const providerUnsupported = status?.providerImplemented === false;
	const configured =
		status?.configured ??
		Boolean(
			rag.baseUrl.trim() && rag.apiKey.trim() && rag.knowledgeBaseId.trim(),
		);
	const apiKeyStored = hasStoredRagApiKey(rag.apiKey);
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
			const result = await ragListKbs({
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
		if (value !== rag.baseUrl) patchRag({ baseUrl: value });
	}, [baseUrlDraft, patchRag, rag.baseUrl]);

	/** Write-only: an empty input keeps the stored secret untouched. */
	const commitApiKey = useCallback(() => {
		const value = apiKeyDraft.trim();
		setTestState("idle");
		if (!value) return;
		setApiKeyDraft("");
		if (value !== rag.apiKey) patchRag({ apiKey: value });
	}, [apiKeyDraft, patchRag, rag.apiKey]);

	const commitKbId = useCallback(() => {
		const value = kbIdDraft.trim();
		setKbIdDraft(value);
		setTestState("idle");
		if (value !== rag.knowledgeBaseId) {
			patchRag({ knowledgeBaseId: value });
		}
	}, [kbIdDraft, patchRag, rag.knowledgeBaseId]);

	const commitMinutes = useCallback(() => {
		const parsed = Number.parseInt(minutesDraft, 10);
		const next = Number.isFinite(parsed)
			? Math.min(24 * 60, Math.max(1, parsed))
			: rag.sync.autoSyncMinutes;
		setMinutesDraft(String(next));
		if (next !== rag.sync.autoSyncMinutes) {
			updateSync({ autoSyncMinutes: next });
		}
	}, [minutesDraft, updateSync, rag.sync.autoSyncMinutes]);

	/* ------------------------------ actions -------------------------------- */

	const handleTest = useCallback(async () => {
		if (!webHost) return;
		setTestState("testing");
		setTestResult(null);
		setTestError(null);
		try {
			const result = await ragTest({
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
				const result = await ragSyncStart(args ?? {});
				if (!result.started) {
					notifyWarning(t("rag.ops.alreadyRunning"));
				} else if (!args?.paper) {
					notifySuccess(t("rag.ops.started", { total: result.total ?? 0 }));
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
			const result = await ragSyncCancel();
			if (result.cancelled) notifySuccess(t("rag.ops.cancelled"));
			else notifyWarning(t("rag.ops.nothingToCancel"));
		} catch (error) {
			notifyError(errorMessage(error));
		} finally {
			setBusy(null);
		}
	}, [t]);

	const runReconcile = useCallback(async () => {
		setBusy("reconcile");
		try {
			const result = await ragReconcile({ prune: false });
			notifySuccess(
				t("rag.ops.reconcileDone", {
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
			const result = await ragClearMapping({
				deleteDocs: clearDeleteDocs,
			});
			notifySuccess(
				t("rag.clear.done", {
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
				<PageTitle title={t("rag.title")} />
				<div className="rounded-xl border border-border/70 bg-muted/30 px-3.5 py-3 text-muted-foreground text-xs leading-relaxed">
					{t("rag.desktopOnly")}
				</div>
			</section>
		);
	}

	const selectedKbName =
		kbs?.find((kb) => kb.id === rag.knowledgeBaseId)?.name ?? null;
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
		? t("rag.status.backendUnavailable")
		: providerUnsupported
			? t("rag.status.providerUnavailable")
			: running
				? t("rag.status.syncing")
				: status?.enabled && configured
					? t("rag.status.connected")
					: status?.enabled
						? t("rag.status.unconfigured")
						: t("rag.status.disabled");
	const statusTone = statusError
		? "err"
		: providerUnsupported
			? "warn"
			: running
				? "warn"
				: status?.enabled && configured
					? "ok"
					: "idle";
	const statusText = status
		? `${synced}/${total}${
				status.sync.lastSyncAt
					? ` · ${t("rag.status.lastSyncAt", {
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
			? t("rag.test.testing")
			: testError
				? testError
				: testResult?.ok
					? t("rag.test.okDetail", {
							kbName: testResult.kbName?.trim() || rag.knowledgeBaseId || "—",
							count: testResult.knowledgeCount ?? 0,
							model: testResult.embeddingModelId?.trim() || "—",
						})
					: testResult?.error?.trim()
						? t("rag.test.failedDetail", { error: testResult.error })
						: t("rag.test.failed");

	const kbDescription = kbError
		? t("rag.kb.error", { error: kbError })
		: kbLoading
			? t("rag.kb.loading")
			: kbs
				? t("rag.kb.loaded", { count: kbs.length })
				: t("rag.kb.help");

	return (
		<section>
			<PageTitle
				title={
					<span className="inline-flex min-w-0 items-center gap-2">
						<Database
							className="size-4 shrink-0 text-muted-foreground"
							aria-hidden
						/>
						<span className="truncate">{t("rag.title")}</span>
						<StatusDot tone={statusTone} label={statusLabel} />
						{statusText ? (
							<span className="truncate font-normal text-muted-foreground text-xs tabular-nums">
								{statusText}
							</span>
						) : null}
					</span>
				}
			/>

			<p className="-mt-2 mb-4 text-muted-foreground text-xs leading-relaxed">
				{t("rag.subtitle")}
			</p>

			{loading ? (
				<p className="mb-4 flex items-center gap-2 text-muted-foreground text-xs">
					<LoaderCircle className="size-3.5 animate-spin" aria-hidden />
					{t("rag.loading")}
				</p>
			) : null}

			{statusError ? (
				<div
					role="alert"
					className="mb-4 rounded-xl border border-amber-500/40 bg-amber-500/5 px-3.5 py-3 text-xs leading-relaxed"
				>
					<p className="flex items-center gap-1.5 font-medium text-amber-700 dark:text-amber-400">
						<CircleAlert className="size-3.5" aria-hidden />
						{isRagUnavailableError(statusError)
							? t("rag.unavailable.title")
							: t("rag.unavailable.errorTitle")}
					</p>
					<p className="mt-1 text-muted-foreground">
						{isRagUnavailableError(statusError)
							? t("rag.unavailable.body")
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
						{t("rag.unavailable.retry")}
					</Button>
				</div>
			) : null}

			<SettingsGroup>
				<SettingsRow
					label={t("rag.provider.label")}
					description={
						providerUnsupported
							? t("rag.provider.unavailable")
							: t("rag.provider.help")
					}
					htmlFor="rag-provider"
				>
					<Select
						value={rag.provider}
						disabled={disabled}
						onValueChange={(value) => {
							if (isRagProviderId(value)) patchRag({ provider: value });
						}}
					>
						<SelectTrigger
							id="rag-provider"
							size="sm"
							className="min-w-[10rem] max-w-[16rem]"
						>
							<SelectValue />
						</SelectTrigger>
						<SelectContent>
							{RAG_PROVIDER_CHOICES.map((choice) => (
								<SelectItem
									key={choice.id}
									value={choice.id}
									disabled={!choice.available}
								>
									{choice.available
										? choice.label
										: `${choice.label} · ${t("rag.provider.comingSoon")}`}
								</SelectItem>
							))}
						</SelectContent>
					</Select>
				</SettingsRow>
				<SettingsRow
					label={t("rag.enable.label")}
					description={t("rag.enable.description")}
					htmlFor="rag-enabled"
				>
					<Switch
						id="rag-enabled"
						checked={rag.enabled}
						disabled={disabled || providerUnsupported}
						onCheckedChange={(enabled) => patchRag({ enabled })}
					/>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("rag.sections.connection")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<WideFieldRow
					label={t("rag.baseUrl.label")}
					description={t("rag.baseUrl.help")}
					htmlFor="rag-base-url"
				>
					<Input
						id="rag-base-url"
						value={baseUrlDraft}
						placeholder="https://weknora.example.com/api/v1"
						className="h-8 w-full min-w-0 flex-1 font-mono text-xs"
						spellCheck={false}
						autoComplete="off"
						disabled={disabled || providerUnsupported}
						onChange={(event) => setBaseUrlDraft(event.currentTarget.value)}
						onBlur={commitBaseUrl}
					/>
				</WideFieldRow>
				<WideFieldRow
					label={t("rag.apiKey.label")}
					description={
						apiKeyStored ? t("rag.apiKey.stored") : t("rag.apiKey.help")
					}
					htmlFor="rag-api-key"
				>
					<Input
						id="rag-api-key"
						type="password"
						value={apiKeyDraft}
						placeholder={
							apiKeyStored
								? t("rag.apiKey.placeholderStored")
								: t("rag.apiKey.placeholder")
						}
						className="h-8 w-full min-w-0 flex-1 font-mono text-xs"
						autoComplete="off"
						disabled={disabled || providerUnsupported}
						onChange={(event) => setApiKeyDraft(event.currentTarget.value)}
						onBlur={commitApiKey}
					/>
					<Button
						type="button"
						variant="outline"
						size="sm"
						className="shrink-0"
						disabled={
							disabled || providerUnsupported || testState === "testing"
						}
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
						{t("rag.test.button")}
					</Button>
				</WideFieldRow>
				{testState !== "idle" && testState !== "testing" ? (
					<SettingsRow label={t("rag.test.resultLabel")}>
						<div className="flex min-w-0 items-center gap-2">
							<StatusDot tone={testTone} label={t("rag.test.resultLabel")} />
							<span
								className={cn(
									"min-w-0 max-w-[36rem] truncate text-xs",
									testTone === "ok"
										? "text-emerald-600 dark:text-emerald-400"
										: "text-destructive",
								)}
								title={testText}
							>
								{testText}
							</span>
						</div>
					</SettingsRow>
				) : null}
				<WideFieldRow
					label={t("rag.kb.label")}
					description={kbDescription}
					htmlFor="rag-kb"
				>
					<Input
						id="rag-kb"
						list="rag-kb-options"
						value={kbIdDraft}
						placeholder={t("rag.kb.placeholder")}
						className="h-8 w-full min-w-0 flex-1 font-mono text-xs"
						spellCheck={false}
						autoComplete="off"
						disabled={disabled || providerUnsupported}
						onChange={(event) => setKbIdDraft(event.currentTarget.value)}
						onBlur={commitKbId}
					/>
					<datalist id="rag-kb-options">
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
						className="size-8 shrink-0"
						aria-label={t("rag.kb.refresh")}
						disabled={disabled || providerUnsupported || kbLoading}
						onClick={() => void loadKbs()}
					>
						{kbLoading ? (
							<LoaderCircle className="size-4 animate-spin" />
						) : (
							<RefreshCw className="size-4" />
						)}
					</Button>
				</WideFieldRow>
			</SettingsGroup>

			<SettingsSectionLabel>{t("rag.sections.content")}</SettingsSectionLabel>
			<SettingsGroup>
				{(
					[
						{
							key: "includePaperBody",
							label: t("rag.content.paperBody"),
						},
						{
							key: "includeNotes",
							label: t("rag.content.notes"),
						},
						{
							key: "includeMarks",
							label: t("rag.content.marks"),
						},
						{
							key: "includeFigureIndex",
							label: t("rag.content.figureIndex"),
						},
						{
							key: "includeCitation",
							label: t("rag.content.citation"),
						},
						{
							key: "ensurePaperBody",
							label: t("rag.content.ensurePaperBody"),
							description: t("rag.content.ensurePaperBodyHelp"),
						},
					] as const
				).map((item) => (
					<ToggleRow
						key={item.key}
						label={item.label}
						description={"description" in item ? item.description : undefined}
						checked={rag.sync[item.key]}
						disabled={disabled || providerUnsupported}
						onCheckedChange={(checked) =>
							updateSync({
								[item.key]: checked,
							} as Partial<RagSyncSettings>)
						}
					/>
				))}
				<div className="space-y-1.5 border-border/50 border-t px-3.5 py-2.5 text-muted-foreground text-xs leading-relaxed">
					<p className="flex items-start gap-1.5">
						<ShieldAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
						<span>{t("rag.privacy.pdf")}</span>
					</p>
					<p>{t("rag.privacy.notes")}</p>
				</div>
			</SettingsGroup>

			<SettingsSectionLabel>{t("rag.sections.autoSync")}</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("rag.autoSync.label")}
					description={t("rag.autoSync.description")}
					htmlFor="rag-auto-sync"
				>
					<Switch
						id="rag-auto-sync"
						checked={rag.sync.autoSync}
						disabled={disabled || providerUnsupported}
						onCheckedChange={(autoSync) => updateSync({ autoSync })}
					/>
				</SettingsRow>
				<SettingsRow
					label={t("rag.autoSync.interval")}
					htmlFor="rag-auto-minutes"
				>
					<div className="flex items-center gap-2">
						<Input
							id="rag-auto-minutes"
							type="number"
							min={1}
							max={24 * 60}
							step={5}
							className="h-8 w-24"
							value={minutesDraft}
							disabled={disabled || providerUnsupported || !rag.sync.autoSync}
							onChange={(event) => setMinutesDraft(event.currentTarget.value)}
							onBlur={commitMinutes}
							onKeyDown={(event) => {
								if (event.key !== "Enter") return;
								event.preventDefault();
								event.currentTarget.blur();
							}}
						/>
						<span className="text-muted-foreground text-xs">
							{t("rag.autoSync.minutesUnit")}
						</span>
					</div>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>{t("rag.sections.tools")}</SettingsSectionLabel>
			<SettingsGroup>
				<ToggleRow
					label={t("rag.tools.agent")}
					description={t("rag.tools.agentHelp")}
					checked={rag.tools.exposeToAgent}
					disabled={disabled || providerUnsupported}
					onCheckedChange={(exposeToAgent) => updateTools({ exposeToAgent })}
				/>
				<ToggleRow
					label={t("rag.tools.mcp")}
					description={t("rag.tools.mcpHelp")}
					checked={rag.tools.exposeToMcp}
					disabled={disabled || providerUnsupported}
					onCheckedChange={(exposeToMcp) => updateTools({ exposeToMcp })}
				/>
			</SettingsGroup>

			<SettingsSectionLabel>{t("rag.sections.actions")}</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("rag.ops.syncLabel")}
					description={
						!rag.enabled
							? t("rag.ops.needEnabled")
							: !configured
								? t("rag.ops.needConfigured")
								: t("rag.ops.syncDetail", { total })
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
								{t("rag.ops.cancel")}
							</Button>
						) : null}
						<Button
							type="button"
							size="sm"
							disabled={
								busy !== null ||
								running ||
								!ready ||
								providerUnsupported ||
								!rag.enabled ||
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
							{t("rag.ops.syncNow")}
						</Button>
					</div>
				</SettingsRow>
				<SettingsRow
					label={t("rag.ops.reconcileLabel")}
					description={t("rag.ops.reconcileDescription")}
				>
					<Button
						type="button"
						variant="outline"
						size="sm"
						disabled={
							busy !== null ||
							running ||
							!ready ||
							providerUnsupported ||
							!configured
						}
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
						{t("rag.ops.reconcile")}
					</Button>
				</SettingsRow>
				<SettingsRow
					label={
						<span className="text-destructive">{t("rag.ops.clearLabel")}</span>
					}
					description={t("rag.ops.clearDescription")}
				>
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy !== null || running || !ready || providerUnsupported}
						onClick={() => setClearOpen(true)}
					>
						<Trash2 data-icon="inline-start" className="size-3.5" />
						{t("rag.ops.clear")}
					</Button>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>{t("rag.sections.status")}</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow label={t("rag.status.provider")}>
					<span className="text-muted-foreground text-xs">
						{currentProviderLabel}
					</span>
				</SettingsRow>
				<SettingsRow label={t("rag.status.label")}>
					<span className="flex flex-wrap items-center justify-end gap-x-3 gap-y-1 text-muted-foreground text-xs tabular-nums">
						<span className="font-medium text-foreground">
							{t("rag.status.synced", { synced, total })}
						</span>
						{indexing > 0 ? (
							<span>{t("rag.status.indexing", { count: indexing })}</span>
						) : null}
						{pending > 0 ? (
							<span>{t("rag.status.pending", { count: pending })}</span>
						) : null}
						{failed > 0 ? (
							<span className="text-destructive">
								{t("rag.status.failed", { count: failed })}
							</span>
						) : null}
					</span>
				</SettingsRow>
				<SettingsRow label={t("rag.status.lastSync")}>
					<span className="text-muted-foreground text-xs tabular-nums">
						{status?.sync.lastSyncAt
							? formatLocaleTimestamp(status.sync.lastSyncAt)
							: t("rag.status.neverSynced")}
					</span>
				</SettingsRow>
				{status?.kbName || selectedKbName ? (
					<SettingsRow label={t("rag.status.kb")}>
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
						{t("rag.status.lastError", { error: lastError })}
					</div>
				) : null}
				{progress ? (
					<div className="border-border/50 border-b px-3.5 py-2.5 last:border-b-0">
						<div className="flex items-center justify-between gap-3 text-xs">
							<span className="font-medium">
								{running
									? t(`rag.sync.phase.${progress.phase}`)
									: `${t("rag.sync.lastRun")} · ${t(
											`rag.sync.phase.${progress.phase}`,
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
									{t("rag.sync.counters.created", {
										count: progressCounters.created,
									})}
								</span>
								<span>
									{t("rag.sync.counters.updated", {
										count: progressCounters.updated,
									})}
								</span>
								<span>
									{t("rag.sync.counters.skipped", {
										count: progressCounters.skipped,
									})}
								</span>
								<span>
									{t("rag.sync.counters.deleted", {
										count: progressCounters.deleted,
									})}
								</span>
								<span
									className={cn(
										progressCounters.failed > 0 && "text-destructive",
									)}
								>
									{t("rag.sync.counters.failed", {
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
								{t("rag.sync.errorsTitle", {
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
											{t("rag.sync.retry")}
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
						<DialogTitle>{t("rag.clear.title")}</DialogTitle>
					</DialogHeader>
					<div className="space-y-2 py-1 text-muted-foreground text-xs leading-relaxed">
						<p>{t("rag.clear.body")}</p>
						<label
							htmlFor="rag-clear-delete-docs"
							className="flex items-start gap-2 text-foreground"
						>
							<Checkbox
								id="rag-clear-delete-docs"
								checked={clearDeleteDocs}
								disabled={busy === "clear"}
								onCheckedChange={(checked) =>
									setClearDeleteDocs(checked === true)
								}
							/>
							<span>
								{t("rag.clear.deleteDocs")}
								<span className="mt-0.5 block text-muted-foreground text-xs">
									{t("rag.clear.deleteDocsHint")}
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
							{t("rag.clear.confirm")}
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</section>
	);
}
