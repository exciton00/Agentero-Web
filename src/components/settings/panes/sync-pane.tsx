import { ChevronRight, CloudUpload, LoaderCircle, Unplug } from "lucide-react";
import type { ComponentType } from "react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FaAws } from "react-icons/fa6";
import {
	SiAlibabacloud,
	SiBaidu,
	SiCloudflare,
	SiGithub,
	SiGitlab,
	SiMinio,
	SiNextcloud,
} from "react-icons/si";
import { JianguoyunIcon } from "@/components/icons/jianguoyun-icon";
import {
	HelpLabel,
	PageTitle,
	SettingsGroup,
	SettingsRow,
	SettingsSectionLabel,
} from "@/components/settings/settings-layout";
import { Button } from "@/components/ui/button";
import {
	Collapsible,
	CollapsibleContent,
	CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Input } from "@/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
	Tooltip,
	TooltipContent,
	TooltipTrigger,
} from "@/components/ui/tooltip";
import { formatLocaleTimestamp } from "@/i18n";
import { formatBytes } from "@/lib/core/background-tasks";
import { events } from "@/lib/core/bindings";
import { errorMessage, notifyError, notifySuccess } from "@/lib/core/notify";
import { openExternalUrl } from "@/lib/core/open-external";
import { hasHostTransport, isWebHost } from "@/lib/core/tauri";
import { toSafeDisposer } from "@/lib/core/tauri-events";
import { cn } from "@/lib/core/utils";
import {
	emptySyncConfig,
	isSyncProgressPhase,
	SYNC_INTERVAL_CHOICES,
	type SyncBackendConfig,
	type SyncBackendKind,
	type SyncOutcome,
	type SyncProgressEvent,
	type SyncScopeSizes,
	type SyncStateEvent,
	type SyncStatus,
	syncConfigure,
	syncDisconnect,
	syncGetStatus,
	syncNow,
	syncScopeSizes,
} from "@/lib/sync/api";
import { isRemoteVaultHandle } from "@/lib/vault/remote/remote-vault";
import { getSavedVaultPath } from "@/lib/vault/session";

/**
 * The headless host returns the persisted last-pass summary, its error and the
 * scheduler's next run on top of the shared `SyncStatus_Serialize` shape.
 * (The desktop commands only carry the live fields, so these stay optional.)
 */
type WebSyncStatus = SyncStatus & {
	lastOutcome?: WebSyncOutcome | null;
	lastError?: string | null;
	autoSyncNextRunAt?: string | null;
};

type WebSyncOutcome = {
	version: number;
	uploaded: number;
	downloaded: number;
	deletedLocal: number;
	removedRemote: number;
	conflictCopies: string[];
	finishedAt: string;
};

type WebSyncScopeSizes = SyncScopeSizes & {
	pdfFiles?: number;
	sourceFiles?: number;
	attachmentFiles?: number;
};

type SyncProviderLink = {
	id: string;
	name: string;
	docsUrl: string;
	icon: ComponentType<{ className?: string }>;
	iconClassName: string;
};

const SYNC_PROVIDER_LINKS: SyncProviderLink[] = [
	{
		id: "aws",
		name: "AWS S3",
		docsUrl:
			"https://docs.aws.amazon.com/AmazonS3/latest/userguide/GetStartedWithS3.html",
		icon: FaAws,
		iconClassName: "text-[#FF9900]",
	},
	{
		id: "cloudflare-r2",
		name: "Cloudflare R2",
		docsUrl: "https://developers.cloudflare.com/r2/get-started/s3/",
		icon: SiCloudflare,
		iconClassName: "text-[#F38020]",
	},
	{
		id: "minio",
		name: "MinIO",
		docsUrl:
			"https://docs.min.io/aistor/administration/console/security-and-access/",
		icon: SiMinio,
		iconClassName: "text-[#C72E49]",
	},
	{
		id: "alibaba-oss",
		name: "Alibaba Cloud OSS",
		docsUrl:
			"https://www.alibabacloud.com/help/en/oss/developer-reference/use-aws-sdks-to-access-oss",
		icon: SiAlibabacloud,
		iconClassName: "text-[#FF6A00]",
	},
	{
		id: "baidu-bos",
		name: "Baidu BOS",
		docsUrl: "https://cloud.baidu.com/doc/BOS/s/ojwvyq973",
		icon: SiBaidu,
		iconClassName: "text-[#2932E1]",
	},
];

const WEBDAV_PROVIDER_LINKS: SyncProviderLink[] = [
	{
		id: "jianguoyun",
		name: "Jianguoyun",
		docsUrl: "https://help.jianguoyun.com/?p=2064",
		icon: JianguoyunIcon,
		iconClassName: "text-[#C7792B]",
	},
	{
		id: "nextcloud",
		name: "Nextcloud",
		docsUrl:
			"https://docs.nextcloud.com/server/latest/user_manual/en/files/access_webdav.html",
		icon: SiNextcloud,
		iconClassName: "text-[#0082C9]",
	},
];

const GIT_PROVIDER_LINKS: SyncProviderLink[] = [
	{
		id: "github",
		name: "GitHub",
		docsUrl:
			"https://docs.github.com/en/repositories/creating-and-managing-repositories/creating-a-new-repository",
		icon: SiGithub,
		iconClassName: "text-foreground",
	},
	{
		id: "gitlab",
		name: "GitLab",
		docsUrl: "https://docs.gitlab.com/ee/user/project/repository/",
		icon: SiGitlab,
		iconClassName: "text-[#FC6D26]",
	},
];

const BACKEND_OPTIONS = [
	{ value: "s3", labelKey: "sync.backendS3" },
	{ value: "webdav", labelKey: "sync.backendWebdav" },
	{ value: "git", labelKey: "sync.backendGit" },
] as const;

const BACKEND_LABEL_KEYS = {
	s3: "sync.backendS3",
	webdav: "sync.backendWebdav",
	git: "sync.backendGit",
} as const satisfies Record<SyncBackendKind, string>;

/**
 * Human-readable remote target for the status card. Secrets never reach this
 * code (the Host masks them), but WebDAV URLs may embed credentials, so strip
 * the userinfo before showing the target.
 */
function remoteTargetLabel(config: SyncBackendConfig): string {
	if (config.backend === "git") {
		return config.gitBranch
			? `${config.gitRemote} · ${config.gitBranch}`
			: config.gitRemote;
	}
	if (config.backend === "webdav") {
		try {
			const url = new URL(config.webdavUrl);
			url.username = "";
			url.password = "";
			return url.toString();
		} catch {
			return config.webdavUrl;
		}
	}
	const bucket = [config.endpoint, config.bucket].filter(Boolean).join(" / ");
	return config.prefix ? `${bucket} / ${config.prefix}` : bucket;
}

/**
 * The settings window is a separate browser tab in the web build, so it has no
 * `vault_path` query parameter and never calls `initVaultStore()`. Resolve the
 * active Vault from the web host instead of declaring cloud sync unavailable.
 * Desktop keeps using the prop only: without a Vault open there, the pane must
 * not silently attach to the last stored path.
 */
function savedWebVaultPath(): string | null {
	if (!isWebHost()) return null;
	const stored = getSavedVaultPath({ allowRestore: true });
	return stored && !isRemoteVaultHandle(stored) ? stored : null;
}

function useSettingsVaultPath(vaultPath: string | null): {
	path: string | null;
	resolving: boolean;
} {
	const initial = vaultPath ?? savedWebVaultPath();
	const [state, setState] = useState<{
		path: string | null;
		resolving: boolean;
	}>({ path: initial, resolving: !initial && isWebHost() });

	useEffect(() => {
		if (vaultPath) {
			setState({ path: vaultPath, resolving: false });
			return;
		}
		if (!isWebHost()) {
			setState({ path: null, resolving: false });
			return;
		}
		const stored = savedWebVaultPath();
		if (stored) {
			setState({ path: stored, resolving: false });
			return;
		}
		let cancelled = false;
		void (async () => {
			try {
				const { bootstrapWebVault } = await import("@/web-transport");
				const root = await bootstrapWebVault();
				if (!cancelled) {
					setState({ path: root ?? null, resolving: false });
				}
			} catch {
				if (!cancelled) setState({ path: null, resolving: false });
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [vaultPath]);

	return state;
}

export function SyncPane({ vaultPath }: { vaultPath: string | null }) {
	const { t } = useTranslation("settings");
	const settingsVault = useSettingsVaultPath(vaultPath);
	const localVault =
		hasHostTransport() &&
		settingsVault.path &&
		!isRemoteVaultHandle(settingsVault.path)
			? settingsVault.path
			: null;

	const [status, setStatus] = useState<WebSyncStatus | null>(null);
	const [form, setForm] = useState<SyncBackendConfig>(emptySyncConfig());
	const [saving, setSaving] = useState(false);
	const [syncing, setSyncing] = useState(false);
	const [progress, setProgress] = useState<SyncProgressEvent | null>(null);
	const [syncError, setSyncError] = useState<string | null>(null);
	const [lastOutcome, setLastOutcome] = useState<SyncOutcome | null>(null);
	// The web settings tab has no native window size; show every S3 field
	// (region / prefix / path-style) without hiding them behind "Advanced".
	const [advancedOpen, setAdvancedOpen] = useState(() => isWebHost());
	const [scopeSizes, setScopeSizes] = useState<WebSyncScopeSizes | null>(null);

	const refresh = useCallback(async () => {
		if (!localVault) return;
		const next = (await syncGetStatus(localVault)) as WebSyncStatus;
		setStatus(next);
		setSyncing(next.running);
		// Persisted summary: shows "last result" after a page refresh/restart
		// instead of only inside the session that ran the sync.
		if (next.lastOutcome) {
			setLastOutcome(next.lastOutcome as unknown as SyncOutcome);
		}
		if (next.config) {
			setForm(next.config);
			// Surface the advanced group when it holds non-default values.
			if (
				next.config.backend === "s3" &&
				(next.config.region !== "us-east-1" ||
					next.config.prefix !== "" ||
					!next.config.forcePathStyle)
			) {
				setAdvancedOpen(true);
			}
		}
	}, [localVault]);

	useEffect(() => {
		void refresh().catch((error) => {
			const message = errorMessage(error);
			setSyncError(message);
			notifyError(message);
		});
	}, [refresh]);

	useEffect(() => {
		if (!localVault) return;
		const offs = [
			toSafeDisposer(
				events.syncState.listen((event) => {
					const payload: SyncStateEvent = event.payload;
					if (payload.vaultPath !== localVault) return;
					setSyncing(payload.status === "syncing");
					setSyncError(
						payload.status === "error"
							? (payload.error ?? t("sync.statusError"))
							: null,
					);
					if (payload.status !== "syncing") {
						setProgress(null);
						void refresh().catch(() => undefined);
					}
				}),
			),
			toSafeDisposer(
				events.syncProgress.listen((event) => {
					const { vaultPath, phase, current, total } = event.payload;
					if (vaultPath === localVault && isSyncProgressPhase(phase)) {
						setProgress({ vaultPath, phase, current, total });
					}
				}),
			),
		];
		return () => {
			for (const off of offs) off();
		};
	}, [localVault, refresh, t]);

	useEffect(() => {
		if (!localVault || form.backend === "git") return;
		let cancelled = false;
		syncScopeSizes(localVault)
			.then((sizes) => {
				if (!cancelled) setScopeSizes(sizes);
			})
			.catch(() => undefined);
		return () => {
			cancelled = true;
		};
	}, [form.backend, localVault]);

	if (!localVault) {
		return (
			<section>
				<PageTitle title={t("sync.title")} />
				<p className="flex items-center gap-2 text-muted-foreground text-sm">
					{settingsVault.resolving ? (
						<LoaderCircle className="size-4 animate-spin" aria-hidden />
					) : null}
					{settingsVault.resolving
						? t("sync.loadingVault")
						: t("sync.localVaultOnly")}
				</p>
			</section>
		);
	}

	const patch = (partial: Partial<SyncBackendConfig>) =>
		setForm((prev) => ({ ...prev, ...partial }));

	const scope = form.scope;
	const patchScope = (key: keyof SyncBackendConfig["scope"], value: boolean) =>
		patch({ scope: { ...scope, [key]: value } });

	const isWebdav = form.backend === "webdav";
	const isGit = form.backend === "git";
	const backendLocked = status?.configured === true;
	const providerLinks = isGit
		? GIT_PROVIDER_LINKS
		: isWebdav
			? WEBDAV_PROVIDER_LINKS
			: SYNC_PROVIDER_LINKS;

	const save = async () => {
		setSaving(true);
		try {
			const next = await syncConfigure(localVault, form);
			setStatus(next);
			if (next.config) setForm(next.config);
			setLastOutcome(null);
			setSyncError(null);
			notifySuccess(t("sync.saved"));
		} catch (error) {
			const message = errorMessage(error);
			setSyncError(message);
			notifyError(message);
		} finally {
			setSaving(false);
		}
	};

	const runSync = async () => {
		setSyncing(true);
		try {
			const outcome = await syncNow(localVault);
			setLastOutcome(outcome);
			setSyncError(null);
			notifySuccess(
				t("sync.done", {
					up: outcome.uploaded,
					down: outcome.downloaded,
				}),
			);
			if (outcome.conflictCopies.length > 0) {
				notifySuccess(
					t("sync.conflicts", { count: outcome.conflictCopies.length }),
				);
			}
		} catch (error) {
			const message = errorMessage(error);
			setLastOutcome(null);
			setSyncError(message);
			notifyError(message);
		} finally {
			setSyncing(false);
			setProgress(null);
			void refresh().catch(() => undefined);
		}
	};

	const disconnect = async () => {
		const previousBackend = status?.config?.backend ?? form.backend;
		try {
			await syncDisconnect(localVault);
			setStatus(null);
			setForm(emptySyncConfig(previousBackend));
			setLastOutcome(null);
			setSyncError(null);
			void refresh().catch(() => undefined);
		} catch (error) {
			const message = errorMessage(error);
			setSyncError(message);
			notifyError(message);
		}
	};

	const field = (
		key: keyof SyncBackendConfig,
		opts?: { type?: string; placeholder?: string },
	) => (
		<Input
			id={`sync-${key}`}
			className="w-64"
			type={opts?.type ?? "text"}
			placeholder={opts?.placeholder}
			value={String(form[key])}
			onChange={(e) => patch({ [key]: e.target.value })}
			autoComplete="off"
			spellCheck={false}
		/>
	);

	const statusText = syncError
		? t("sync.statusError")
		: syncing && progress
			? t(`sync.phase.${progress.phase}`, {
					current: progress.current,
					total: progress.total,
				})
			: status?.lastSyncAt
				? t("sync.lastSync", {
						time: formatLocaleTimestamp(status.lastSyncAt),
					})
				: status?.configured
					? t("sync.neverSynced")
					: status
						? t("sync.statusDisconnected")
						: null;
	const syncStatusLabel = syncError
		? t("sync.statusError")
		: syncing
			? t("sync.statusSyncing")
			: status?.configured
				? t("sync.statusConnected")
				: t("sync.statusDisconnected");
	const syncStatusDotClass = syncError
		? "bg-destructive"
		: syncing
			? "bg-sky-500"
			: status?.configured
				? "bg-emerald-500"
				: "bg-muted-foreground/35";
	const configChanged = JSON.stringify(form) !== JSON.stringify(status?.config);
	const savedConfig = status?.config ?? null;
	const remoteTarget = savedConfig ? remoteTargetLabel(savedConfig) : "";
	const primaryAction =
		!status?.configured || configChanged ? (
			<Button
				type="button"
				size="sm"
				onClick={() => void save()}
				disabled={saving || syncing}
			>
				{saving ? (
					<LoaderCircle data-icon="inline-start" className="animate-spin" />
				) : null}
				{status?.configured ? t("sync.save") : t("sync.connect")}
			</Button>
		) : (
			<Button
				type="button"
				size="sm"
				onClick={() => void runSync()}
				disabled={syncing}
			>
				{syncing ? (
					<LoaderCircle data-icon="inline-start" className="animate-spin" />
				) : (
					<CloudUpload data-icon="inline-start" />
				)}
				{t("sync.syncNow")}
			</Button>
		);

	return (
		<section>
			<PageTitle
				title={
					<>
						<span className="inline-flex items-center gap-2">
							{t("sync.title")}
							<span
								role="status"
								aria-label={syncStatusLabel}
								title={syncError ?? syncStatusLabel}
								className={cn("size-2 rounded-full", syncStatusDotClass)}
							/>
						</span>
						{statusText ? (
							<span className="ml-2 font-normal text-muted-foreground text-xs">
								{statusText}
							</span>
						) : null}
					</>
				}
				actions={primaryAction}
			/>

			{syncError ? (
				<p
					role="alert"
					className="mb-4 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-destructive text-xs leading-relaxed"
				>
					{syncError}
				</p>
			) : status?.lastError ? (
				<p
					role="alert"
					className="mb-4 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-destructive text-xs leading-relaxed"
				>
					{t("sync.lastError", { error: status.lastError })}
				</p>
			) : null}

			{status?.configured && savedConfig ? (
				<SettingsGroup>
					<SettingsRow label={t("sync.backend")}>
						<span className="text-muted-foreground text-sm">
							{t(BACKEND_LABEL_KEYS[savedConfig.backend])}
						</span>
					</SettingsRow>
					<SettingsRow label={t("sync.remoteTarget")}>
						<span
							className="block max-w-[26rem] truncate text-muted-foreground text-sm"
							title={remoteTarget}
						>
							{remoteTarget}
						</span>
					</SettingsRow>
					<SettingsRow label={t("sync.lastSyncLabel")}>
						<span className="flex items-center gap-2 text-muted-foreground text-xs tabular-nums">
							<span>
								{status.lastSyncAt
									? formatLocaleTimestamp(status.lastSyncAt)
									: t("sync.neverSynced")}
							</span>
							{status.lastVersion > 0 ? (
								<span className="text-foreground/60">
									{t("sync.resultVersion", {
										version: status.lastVersion,
									})}
								</span>
							) : null}
						</span>
					</SettingsRow>
					<SettingsRow label={t("sync.lastResult")}>
						{lastOutcome ? (
							<span className="flex max-w-[34rem] flex-wrap items-center justify-end gap-x-3 gap-y-1 text-muted-foreground text-xs tabular-nums">
								<span className="font-medium text-foreground">
									{t("sync.resultVersion", {
										version: lastOutcome.version,
									})}
								</span>
								<span>
									{t("sync.resultUploaded", {
										count: lastOutcome.uploaded,
									})}
								</span>
								<span>
									{t("sync.resultDownloaded", {
										count: lastOutcome.downloaded,
									})}
								</span>
								<span>
									{t("sync.resultDeletedLocal", {
										count: lastOutcome.deletedLocal,
									})}
								</span>
								<span>
									{t("sync.resultRemovedRemote", {
										count: lastOutcome.removedRemote,
									})}
								</span>
								<span
									className={cn(
										lastOutcome.conflictCopies.length > 0 &&
											"text-amber-600 dark:text-amber-500",
									)}
								>
									{t("sync.resultConflicts", {
										count: lastOutcome.conflictCopies.length,
									})}
								</span>
							</span>
						) : (
							<span className="text-muted-foreground text-xs">
								{t("sync.noRecentResult")}
							</span>
						)}
					</SettingsRow>
				</SettingsGroup>
			) : null}

			{status?.configured && !isGit && !form.conditionalWrites ? (
				<p className="mb-3 text-muted-foreground text-xs">
					{t("sync.noConditionalWrites")}
				</p>
			) : null}

			<div className="mb-3 flex flex-wrap items-center gap-2">
				<span id="sync-backend-label" className="text-muted-foreground text-xs">
					{t("sync.backend")}
				</span>
				<Tooltip>
					<TooltipTrigger asChild>
						<span className="inline-flex">
							<Select
								value={form.backend}
								onValueChange={(value) => {
									if (value === "s3" || value === "webdav" || value === "git") {
										patch({ backend: value });
									}
								}}
								disabled={backendLocked}
							>
								<SelectTrigger
									id="sync-backend"
									size="sm"
									className="w-36"
									aria-labelledby="sync-backend-label"
								>
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									{BACKEND_OPTIONS.map((option) => (
										<SelectItem key={option.value} value={option.value}>
											{t(option.labelKey)}
										</SelectItem>
									))}
								</SelectContent>
							</Select>
						</span>
					</TooltipTrigger>
					{backendLocked ? (
						<TooltipContent side="bottom">
							{t("sync.backendLocked")}
						</TooltipContent>
					) : null}
				</Tooltip>
				<div className="ml-auto flex flex-wrap items-center gap-1.5">
					{providerLinks.map((provider) => {
						const Icon = provider.icon;
						return (
							<Tooltip key={provider.id}>
								<TooltipTrigger asChild>
									<Button
										type="button"
										size="icon-lg"
										variant="outline"
										className="size-9 bg-background"
										aria-label={t("sync.providerDocsAria", {
											name: provider.name,
										})}
										onClick={() => openExternalUrl(provider.docsUrl)}
									>
										<Icon
											className={cn("size-5", provider.iconClassName)}
											aria-hidden
										/>
									</Button>
								</TooltipTrigger>
								<TooltipContent side="bottom">{provider.name}</TooltipContent>
							</Tooltip>
						);
					})}
				</div>
			</div>

			{isGit ? (
				<SettingsGroup>
					<SettingsRow
						label={t("sync.gitRemote")}
						htmlFor="sync-gitRemote"
						description={t("sync.gitRemoteHint")}
					>
						{field("gitRemote", {
							placeholder: "git@github.com:you/papers.git",
						})}
					</SettingsRow>
					<SettingsRow label={t("sync.gitBranch")} htmlFor="sync-gitBranch">
						{field("gitBranch", { placeholder: "main" })}
					</SettingsRow>
				</SettingsGroup>
			) : isWebdav ? (
				<SettingsGroup>
					<SettingsRow label={t("sync.serverUrl")} htmlFor="sync-webdavUrl">
						{field("webdavUrl", {
							placeholder: "https://dav.jianguoyun.com/dav/agentero",
						})}
					</SettingsRow>
					<SettingsRow label={t("sync.username")} htmlFor="sync-webdavUsername">
						{field("webdavUsername")}
					</SettingsRow>
					<SettingsRow label={t("sync.password")} htmlFor="sync-webdavPassword">
						{field("webdavPassword", { type: "password" })}
					</SettingsRow>
				</SettingsGroup>
			) : (
				<SettingsGroup>
					<SettingsRow label={t("sync.endpoint")} htmlFor="sync-endpoint">
						{field("endpoint", { placeholder: "https://…" })}
					</SettingsRow>
					<SettingsRow label={t("sync.bucket")} htmlFor="sync-bucket">
						{field("bucket")}
					</SettingsRow>
					<SettingsRow label={t("sync.accessKey")} htmlFor="sync-accessKey">
						{field("accessKey")}
					</SettingsRow>
					<SettingsRow label={t("sync.secretKey")} htmlFor="sync-secretKey">
						{field("secretKey", { type: "password" })}
					</SettingsRow>
				</SettingsGroup>
			)}

			<SettingsGroup>
				<SettingsRow
					label={t("sync.autoSync")}
					htmlFor="sync-autoSync"
					description={
						isWebHost()
							? form.autoSync && status?.autoSyncNextRunAt
								? t("sync.autoSyncNextRun", {
										time: formatLocaleTimestamp(status.autoSyncNextRunAt),
									})
								: t("sync.autoSyncWebHint")
							: undefined
					}
				>
					<Switch
						id="sync-autoSync"
						checked={form.autoSync}
						onCheckedChange={(checked) => patch({ autoSync: checked })}
					/>
				</SettingsRow>
				{form.autoSync ? (
					<SettingsRow label={t("sync.interval")} htmlFor="sync-interval">
						<Select
							value={String(form.intervalMinutes)}
							onValueChange={(value) =>
								patch({ intervalMinutes: Number(value) })
							}
						>
							<SelectTrigger id="sync-interval" size="sm" className="w-32">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{SYNC_INTERVAL_CHOICES.map((minutes) => (
									<SelectItem key={minutes} value={String(minutes)}>
										{t("sync.intervalMinutes", { count: minutes })}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</SettingsRow>
				) : null}
			</SettingsGroup>

			{!isGit ? (
				<>
					<SettingsSectionLabel className="mt-4">
						<HelpLabel label={t("sync.scope")} help={t("sync.scopeHint")} />
					</SettingsSectionLabel>
					<SettingsGroup>
						<SettingsRow label={t("sync.scopePdf")} htmlFor="sync-scope-pdf">
							<span className="flex items-center gap-2">
								{scopeSizes ? (
									<span className="text-muted-foreground text-xs tabular-nums">
										{t("sync.scopeSize", {
											files: scopeSizes.pdfFiles ?? 0,
											size: formatBytes(scopeSizes.pdf),
										})}
									</span>
								) : null}
								<Switch
									id="sync-scope-pdf"
									checked={scope.pdf}
									onCheckedChange={(checked) => patchScope("pdf", checked)}
								/>
							</span>
						</SettingsRow>
						<SettingsRow
							label={t("sync.scopeSource")}
							htmlFor="sync-scope-source"
						>
							<span className="flex items-center gap-2">
								{scopeSizes ? (
									<span className="text-muted-foreground text-xs tabular-nums">
										{t("sync.scopeSize", {
											files: scopeSizes.sourceFiles ?? 0,
											size: formatBytes(scopeSizes.source),
										})}
									</span>
								) : null}
								<Switch
									id="sync-scope-source"
									checked={scope.source}
									onCheckedChange={(checked) => patchScope("source", checked)}
								/>
							</span>
						</SettingsRow>
						<SettingsRow
							label={t("sync.scopeAttachments")}
							htmlFor="sync-scope-attachments"
						>
							<span className="flex items-center gap-2">
								{scopeSizes ? (
									<span className="text-muted-foreground text-xs tabular-nums">
										{t("sync.scopeSize", {
											files: scopeSizes.attachmentFiles ?? 0,
											size: formatBytes(scopeSizes.attachments),
										})}
									</span>
								) : null}
								<Switch
									id="sync-scope-attachments"
									checked={scope.attachments}
									onCheckedChange={(checked) =>
										patchScope("attachments", checked)
									}
								/>
							</span>
						</SettingsRow>
					</SettingsGroup>
				</>
			) : null}

			{!isWebdav && !isGit ? (
				<Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
					<CollapsibleTrigger asChild>
						<button
							type="button"
							className="mb-2 flex items-center gap-1 text-muted-foreground text-xs outline-none transition-colors hover:text-foreground"
							aria-label={t("sync.advanced")}
						>
							<ChevronRight
								className={cn(
									"size-3.5 transition-transform",
									advancedOpen && "rotate-90",
								)}
							/>
							{t("sync.advanced")}
						</button>
					</CollapsibleTrigger>
					<CollapsibleContent>
						<SettingsGroup>
							<SettingsRow label={t("sync.region")} htmlFor="sync-region">
								{field("region")}
							</SettingsRow>
							<SettingsRow
								label={
									<HelpLabel
										label={t("sync.prefix")}
										help={t("sync.prefixHint")}
									/>
								}
								htmlFor="sync-prefix"
							>
								{field("prefix")}
							</SettingsRow>
							<SettingsRow
								label={
									<HelpLabel
										label={t("sync.pathStyle")}
										help={t("sync.pathStyleHint")}
									/>
								}
								htmlFor="sync-pathStyle"
							>
								<Switch
									id="sync-pathStyle"
									checked={form.forcePathStyle}
									onCheckedChange={(checked) =>
										patch({ forcePathStyle: checked })
									}
								/>
							</SettingsRow>
						</SettingsGroup>
					</CollapsibleContent>
				</Collapsible>
			) : null}

			<div className="flex items-center gap-2">
				{status?.configured ? (
					<Button
						type="button"
						size="sm"
						variant="ghost"
						onClick={() => void disconnect()}
						disabled={syncing}
					>
						<Unplug data-icon="inline-start" />
						{t("sync.disconnect")}
					</Button>
				) : null}
			</div>
		</section>
	);
}
