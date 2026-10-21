import { invoke } from "@tauri-apps/api/core";
import {
	Check,
	ChevronsUpDown,
	CircleAlert,
	ExternalLink,
	Loader2,
	RefreshCw,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
	PROVIDER_INPUT_CLASS,
	ProbeDot,
	ProviderCard,
	ProviderCardHeader,
	ProviderFieldRow,
} from "@/components/settings/provider-card";
import {
	PageTitle,
	SettingsGroup,
	SettingsRow,
	SettingsSectionLabel,
} from "@/components/settings/settings-layout";
import { Button } from "@/components/ui/button";
import {
	Command,
	CommandEmpty,
	CommandGroup,
	CommandInput,
	CommandItem,
	CommandList,
} from "@/components/ui/command";
import { Input } from "@/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/components/ui/popover";
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
import { useVaultStore } from "@/hooks/use-app-stores";
import { commands } from "@/lib/core/bindings";
import { errorText } from "@/lib/core/error";
import { callApiResult } from "@/lib/core/ipc";
import { notifyError } from "@/lib/core/notify";
import { openExternalUrl } from "@/lib/core/open-external";
import { hasHostTransport, isWebHost } from "@/lib/core/tauri";
import { cn } from "@/lib/core/utils";
import { saveSettingsAsync } from "@/lib/settings";
import {
	DECISION_PROVIDER_PRESETS,
	DEFAULT_BIBTEX_MOUNT_SETTINGS,
	DEFAULT_DECISION_BASE_URL,
} from "@/lib/settings/defaults";
import {
	type AppSettings,
	type BibtexMountSettings,
	DECISION_PROVIDER_IDS,
	type DecisionProviderId,
	type DecisionSettings,
	isBibtexMountFormat,
} from "@/lib/settings/types";
import type { ProbeStatus } from "@/lib/ui/probe-status";
import { getSavedVaultPath } from "@/lib/vault/session";
import { loadVaultTree } from "@/lib/vault/tree";
import type { FileNode } from "@/lib/vault/types";

/** Same convention the Host uses for redacted secrets: all `*`, same length. */
function isMask(value: string): boolean {
	const trimmed = value.trim();
	return trimmed.length > 0 && trimmed.split("").every((c) => c === "*");
}

function maskKey(value: string): string {
	return value ? "*".repeat(value.length) : "";
}

/** Provider-specific endpoint hint; the URL itself is not translated. */
function baseUrlPlaceholder(provider: DecisionProviderId): string {
	if (provider === "clef") {
		return "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/@cf/cloudflare/clef";
	}
	return DEFAULT_DECISION_BASE_URL;
}

type BibtexTagOption = {
	name: string;
	color?: string;
	count?: number;
};

type MountSelectOption = {
	value: string;
	label: string;
	color?: string;
};

type LoadStatus = "loading" | "ready" | "unavailable";
type SaveStatus = "idle" | "saving" | "saved" | "error";

/** Paper-internal buckets are not useful as bibliography scopes. */
const PAPER_INTERNAL_DIR_NAMES = new Set([
	"source",
	"assets",
	"marks",
	"attachments",
]);

/** Coerce a Host / hand-edited value into the UI shape (Host parity). */
function normalizeBibtexMount(raw: unknown): BibtexMountSettings {
	const value =
		raw && typeof raw === "object"
			? (raw as Record<string, unknown>)
			: ({} as Record<string, unknown>);
	// Host normalize: trim, case-insensitive de-dup, first casing wins.
	const tags: string[] = [];
	if (Array.isArray(value.tags)) {
		for (const rawTag of value.tags) {
			if (typeof rawTag !== "string") continue;
			const tag = rawTag.trim();
			if (!tag) continue;
			if (tags.some((tag2) => tag2.toLowerCase() === tag.toLowerCase())) {
				continue;
			}
			tags.push(tag);
		}
	}
	// Host normalize: relative `papers/…` scope only, no traversal.
	const dirs: string[] = [];
	if (Array.isArray(value.dirs)) {
		for (const rawDir of value.dirs) {
			if (typeof rawDir !== "string") continue;
			const trimmed = rawDir.trim().replace(/\\/g, "/");
			if (!trimmed || trimmed.startsWith("/") || /^[A-Za-z]:\//.test(trimmed)) {
				continue;
			}
			const dir = trimmed.replace(/^\/+|\/+$/g, "");
			if (!dir || dir.split("/").some((segment) => segment === "..")) {
				continue;
			}
			if (dir !== "papers" && !dir.startsWith("papers/")) continue;
			if (dirs.includes(dir)) continue;
			dirs.push(dir);
		}
	}
	const rawFormat =
		typeof value.format === "string" ? value.format.trim().toLowerCase() : "";
	return {
		enabled: value.enabled === true,
		format: isBibtexMountFormat(rawFormat)
			? rawFormat
			: DEFAULT_BIBTEX_MOUNT_SETTINGS.format,
		tags,
		dirs,
	};
}

/** Order-insensitive equality so Host echo normalization is not "unsupported". */
function sameBibtexMount(
	a: BibtexMountSettings,
	b: BibtexMountSettings,
): boolean {
	if (a.enabled !== b.enabled || a.format !== b.format) return false;
	if (a.tags.length !== b.tags.length || a.dirs.length !== b.dirs.length) {
		return false;
	}
	const tags = [...a.tags].sort();
	const otherTags = [...b.tags].sort();
	for (let i = 0; i < tags.length; i++) {
		if (tags[i] !== otherTags[i]) return false;
	}
	const dirs = [...a.dirs].sort();
	const otherDirs = [...b.dirs].sort();
	for (let i = 0; i < dirs.length; i++) {
		if (dirs[i] !== otherDirs[i]) return false;
	}
	return true;
}

/**
 * Unwrap the transport envelopes a not-yet-bound Host command may use: the
 * web server returns a plain `ApiResult` (`{ok, data | error}`), while a
 * future typed binding could add one `{status, data}` layer.
 */
function unwrapRpcPayload(value: unknown): unknown {
	let current = value;
	for (let depth = 0; depth < 4; depth++) {
		if (!current || typeof current !== "object" || Array.isArray(current)) {
			return current;
		}
		const record = current as Record<string, unknown>;
		if (record.status === "error") {
			throw new Error(
				typeof record.error === "string" ? record.error : "RPC failed",
			);
		}
		if (record.status === "ok" && "data" in record) {
			current = record.data;
			continue;
		}
		if (typeof record.ok === "boolean") {
			if (!record.ok) {
				const error = record.error;
				const message =
					typeof error === "string"
						? error
						: error &&
								typeof error === "object" &&
								typeof (error as { message?: unknown }).message === "string"
							? (error as { message: string }).message
							: "RPC failed";
				throw new Error(message);
			}
			if ("data" in record) {
				current = record.data;
				continue;
			}
		}
		return current;
	}
	return current;
}

function parseTagsIndex(payload: unknown): BibtexTagOption[] {
	const record =
		payload && typeof payload === "object" && !Array.isArray(payload)
			? (payload as Record<string, unknown>)
			: null;
	const rawTags = Array.isArray(payload)
		? payload
		: record && Array.isArray(record.tags)
			? record.tags
			: null;
	if (!rawTags) {
		throw new Error("tags_index_read returned an unexpected payload");
	}
	const byName = new Map<string, BibtexTagOption>();
	for (const item of rawTags) {
		if (typeof item === "string") {
			const name = item.trim();
			if (name) byName.set(name, { name });
			continue;
		}
		if (!item || typeof item !== "object") continue;
		const tag = item as Record<string, unknown>;
		const name = typeof tag.name === "string" ? tag.name.trim() : "";
		if (!name) continue;
		byName.set(name, {
			name,
			color:
				typeof tag.color === "string" && tag.color.trim()
					? tag.color.trim()
					: undefined,
			count:
				typeof tag.count === "number" && Number.isFinite(tag.count)
					? tag.count
					: undefined,
		});
	}
	return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * The tag index RPC is implemented alongside this UI, so it is intentionally
 * called through the string invoke path instead of the generated bindings.
 */
async function readTagsIndex(vaultPath: string): Promise<BibtexTagOption[]> {
	if (!hasHostTransport()) throw new Error("Host unavailable");
	const raw = await invoke<unknown>("tags_index_read", {
		args: { vaultPath },
	});
	return parseTagsIndex(unwrapRpcPayload(raw));
}

/**
 * Directory scopes under `papers/`, vault-relative. Includes paper folders
 * themselves but skips their internal `source/` / `assets/` / `marks/` buckets.
 */
function collectPaperDirs(nodes: FileNode[]): string[] | null {
	const papers = nodes.find(
		(node) => node.kind === "directory" && node.name === "papers",
	);
	if (!papers) return null;
	const dirs = ["papers"];
	const walk = (node: FileNode, rel: string) => {
		for (const child of node.children ?? []) {
			if (child.kind !== "directory") continue;
			if (PAPER_INTERNAL_DIR_NAMES.has(child.name.toLowerCase())) continue;
			const childRel = `${rel}/${child.name}`;
			dirs.push(childRel);
			if (!child.childrenPending) walk(child, childRel);
		}
	};
	walk(papers, "papers");
	return dirs;
}

/** Compact trigger label: at most two names, then a +N suffix. */
function selectionSummary(values: string[], allLabel: string): string {
	if (values.length === 0) return allLabel;
	const names = values
		.slice(0, 2)
		.map((value) =>
			value.includes("/") ? value.slice(value.lastIndexOf("/") + 1) : value,
		);
	const suffix = values.length > 2 ? ` +${values.length - 2}` : "";
	return `${names.join(", ")}${suffix}`;
}

function MountMultiSelect({
	id,
	options,
	selected,
	onChange,
	allLabel,
	emptyLabel,
	searchPlaceholder,
	refreshLabel,
	status,
	onRefresh,
}: {
	id: string;
	options: MountSelectOption[];
	selected: string[];
	onChange: (next: string[]) => void;
	allLabel: string;
	emptyLabel: string;
	searchPlaceholder: string;
	refreshLabel: string;
	status: LoadStatus;
	onRefresh: () => void;
}) {
	const [open, setOpen] = useState(false);
	const selectedSet = useMemo(() => new Set(selected), [selected]);
	const summary = selectionSummary(selected, allLabel);
	const title = selected.length > 0 ? selected.join(", ") : allLabel;
	return (
		<div className="flex items-center gap-1">
			<Popover open={open} onOpenChange={setOpen}>
				<PopoverTrigger asChild>
					<Button
						id={id}
						type="button"
						variant="outline"
						size="sm"
						role="combobox"
						aria-expanded={open}
						title={title}
						className="h-8 min-w-[150px] max-w-[210px] justify-between gap-1 font-normal"
					>
						<span className="truncate">{summary}</span>
						{status === "loading" ? (
							<Loader2 className="size-3.5 shrink-0 animate-spin opacity-60" />
						) : (
							<ChevronsUpDown className="size-3.5 shrink-0 opacity-50" />
						)}
					</Button>
				</PopoverTrigger>
				<PopoverContent
					align="end"
					className="w-[300px] p-0"
					onOpenAutoFocus={(event) => event.preventDefault()}
				>
					<Command>
						<CommandInput placeholder={searchPlaceholder} />
						<CommandList className="max-h-[min(50vh,300px)]">
							<CommandEmpty>{emptyLabel}</CommandEmpty>
							<CommandGroup>
								<CommandItem
									value={`__all__ ${allLabel}`}
									data-checked={selected.length === 0 || undefined}
									onSelect={() => {
										if (selected.length > 0) onChange([]);
									}}
								>
									<span className="truncate">{allLabel}</span>
								</CommandItem>
								{options.map((option) => {
									const checked = selectedSet.has(option.value);
									return (
										<CommandItem
											key={option.value}
											value={option.value}
											data-checked={checked || undefined}
											onSelect={() =>
												onChange(
													checked
														? selected.filter((value) => value !== option.value)
														: [...selected, option.value],
												)
											}
										>
											{option.color ? (
												<span
													aria-hidden
													className="size-2 shrink-0 rounded-full border border-black/10"
													style={{ backgroundColor: option.color }}
												/>
											) : null}
											<span className="truncate" title={option.label}>
												{option.label}
											</span>
										</CommandItem>
									);
								})}
							</CommandGroup>
						</CommandList>
					</Command>
				</PopoverContent>
			</Popover>
			<Button
				type="button"
				variant="ghost"
				size="icon-xs"
				className="shrink-0"
				aria-label={refreshLabel}
				title={refreshLabel}
				disabled={status === "loading"}
				onClick={onRefresh}
			>
				<RefreshCw
					className={cn("size-3.5", status === "loading" && "animate-spin")}
				/>
			</Button>
		</div>
	);
}

export function ExperimentalPane({
	settings,
	patch,
}: {
	settings: AppSettings;
	patch: (p: Partial<AppSettings>) => void;
}) {
	const { t } = useTranslation("settings");
	const decision = settings.decision;
	const configured = decision.apiKey.trim().length > 0;
	const [probeStatus, setProbeStatus] = useState<ProbeStatus>("idle");
	const [draft, setDraft] = useState<Partial<DecisionSettings>>({});
	const [busy, setBusy] = useState(false);

	const provider = draft.provider ?? decision.provider;
	const preset = DECISION_PROVIDER_PRESETS[provider];

	const displayApiKey =
		draft.apiKey !== undefined
			? draft.apiKey
			: decision.apiKey
				? isMask(decision.apiKey)
					? decision.apiKey
					: maskKey(decision.apiKey)
				: "";

	const displayBaseUrl =
		draft.baseUrl !== undefined ? draft.baseUrl : decision.baseUrl;
	const displayModel = draft.model !== undefined ? draft.model : decision.model;

	const handleProviderChange = useCallback((value: string) => {
		const next = value as DecisionProviderId;
		const nextPreset = DECISION_PROVIDER_PRESETS[next];
		// Switching preset resets the endpoint/model to that provider's defaults;
		// the user can still edit them afterwards.
		setDraft((prev) => ({
			...prev,
			provider: next,
			baseUrl: nextPreset.baseUrl,
			model: nextPreset.model,
		}));
	}, []);

	const handleConfirm = useCallback(async () => {
		const nextSettings: AppSettings = {
			...settings,
			decision: {
				provider,
				apiKey: (draft.apiKey ?? decision.apiKey).trim(),
				baseUrl: (draft.baseUrl ?? decision.baseUrl).trim(),
				model: (draft.model ?? decision.model).trim(),
				smartHighlight: decision.smartHighlight,
			},
		};

		setBusy(true);
		try {
			const saved = await saveSettingsAsync(nextSettings);
			patch({ decision: saved.decision });
			setDraft({});

			if (!saved.decision.apiKey) {
				setProbeStatus("idle");
				return;
			}

			setProbeStatus("probing");
			try {
				await callApiResult(() => commands.jevProbeHealth());
				setProbeStatus("ok");
			} catch (err) {
				setProbeStatus("fail");
				notifyError(errorText(err));
			}
		} catch (err) {
			notifyError(errorText(err));
		} finally {
			setBusy(false);
		}
	}, [decision, draft, patch, provider, settings]);

	const statusLabel = configured
		? t(`experimental.decision.probeStatus.${probeStatus}`)
		: t("experimental.decision.probeStatus.unconfigured");

	return (
		<div className="space-y-6">
			<PageTitle title={t("experimental.title")} />

			<SettingsGroup>
				<SettingsRow
					label={t("experimental.decision.smartHighlight")}
					htmlFor="decision-smart-highlight"
				>
					<Switch
						id="decision-smart-highlight"
						checked={decision.smartHighlight}
						onCheckedChange={(v) =>
							patch({ decision: { ...decision, smartHighlight: v } })
						}
					/>
				</SettingsRow>
			</SettingsGroup>

			<ProviderCard>
				<ProviderCardHeader
					left={
						<>
							<Tooltip>
								<TooltipTrigger asChild>
									<ProbeDot
										status={probeStatus}
										configured={configured}
										label={statusLabel}
									/>
								</TooltipTrigger>
								<TooltipContent>{statusLabel}</TooltipContent>
							</Tooltip>
							<span className="truncate font-medium text-sm">
								{t("experimental.decision.section")}
							</span>
							{preset.keyUrl ? (
								<Button
									type="button"
									variant="link"
									size="xs"
									className="-ml-1.5 h-auto shrink-0 px-1.5 text-primary"
									onClick={() => openExternalUrl(preset.keyUrl)}
								>
									<ExternalLink data-icon="inline-start" className="size-3" />
									{t("experimental.decision.getKey")}
								</Button>
							) : null}
						</>
					}
					right={
						<Button
							type="button"
							variant="outline"
							size="xs"
							disabled={busy}
							onClick={() => void handleConfirm()}
						>
							{busy ? (
								<Loader2 className="mr-1.5 size-3.5 animate-spin" aria-hidden />
							) : null}
							{t("experimental.decision.test")}
						</Button>
					}
				/>

				<div className="grid gap-1.5">
					<ProviderFieldRow
						label={t("experimental.decision.provider.label")}
						htmlFor="decision-provider"
					>
						<Select value={provider} onValueChange={handleProviderChange}>
							<SelectTrigger
								id="decision-provider"
								size="sm"
								className="h-8 min-w-0 flex-1"
							>
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{DECISION_PROVIDER_IDS.map((id) => (
									<SelectItem key={id} value={id}>
										{t(`experimental.decision.provider.${id}`)}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</ProviderFieldRow>

					<ProviderFieldRow
						label={t("experimental.decision.apiKey.label")}
						htmlFor="decision-api-key"
					>
						<Input
							id="decision-api-key"
							type="password"
							value={displayApiKey}
							placeholder={t("experimental.decision.apiKey.placeholder")}
							className={PROVIDER_INPUT_CLASS}
							spellCheck={false}
							autoComplete="off"
							onChange={(e) =>
								setDraft((prev) => ({ ...prev, apiKey: e.target.value }))
							}
							onFocus={(e) => e.target.select()}
						/>
					</ProviderFieldRow>

					<ProviderFieldRow
						label={t("experimental.decision.baseUrl.label")}
						htmlFor="decision-base-url"
					>
						<Input
							id="decision-base-url"
							type="text"
							value={displayBaseUrl}
							placeholder={baseUrlPlaceholder(provider)}
							className={PROVIDER_INPUT_CLASS}
							spellCheck={false}
							autoComplete="off"
							onChange={(e) =>
								setDraft((prev) => ({ ...prev, baseUrl: e.target.value }))
							}
						/>
					</ProviderFieldRow>

					<ProviderFieldRow
						label={t("experimental.decision.model.label")}
						htmlFor="decision-model"
					>
						<Input
							id="decision-model"
							type="text"
							value={displayModel}
							placeholder={t("experimental.decision.model.placeholder")}
							className={PROVIDER_INPUT_CLASS}
							spellCheck={false}
							autoComplete="off"
							onChange={(e) =>
								setDraft((prev) => ({ ...prev, model: e.target.value }))
							}
						/>
					</ProviderFieldRow>
				</div>
			</ProviderCard>

			{isWebHost() ? (
				<BibtexMountSection settings={settings} patch={patch} />
			) : null}
		</div>
	);
}

/** Web-only HTTP bibliography mount (BibTeX / BibLaTeX) controls. */
function BibtexMountSection({
	settings,
	patch,
}: {
	settings: AppSettings;
	patch: (p: Partial<AppSettings>) => void;
}) {
	const { t } = useTranslation("settings");
	const [mount, setMount] = useState<BibtexMountSettings>(() =>
		normalizeBibtexMount(settings.bibtexMount),
	);
	const [saveStatus, setSaveStatus] = useState<SaveStatus>("idle");
	/**
	 * `null` = not exercised yet. After a save that the Host echoes back
	 * without the field (older Host), this becomes false and the pane stops
	 * trusting `settings.bibtexMount` so broadcasts cannot flip the UI back.
	 */
	const [hostSupported, setHostSupported] = useState<boolean | null>(null);
	const [tags, setTags] = useState<BibtexTagOption[]>([]);
	const [tagStatus, setTagStatus] = useState<LoadStatus>("loading");
	const [tagReload, setTagReload] = useState(0);
	const [dirs, setDirs] = useState<string[]>([]);
	const [dirStatus, setDirStatus] = useState<LoadStatus>("loading");
	const [dirReload, setDirReload] = useState(0);

	const storeVaultPath = useVaultStore((state) => state.vaultPath);
	const vaultPath = useMemo(
		() => storeVaultPath ?? getSavedVaultPath(),
		[storeVaultPath],
	);
	const storeTree = useVaultStore((state) => state.tree);

	// Latest snapshots for async saves / external updates.
	const settingsRef = useRef(settings);
	settingsRef.current = settings;
	const patchRef = useRef(patch);
	patchRef.current = patch;
	const saveQueueRef = useRef<Promise<void>>(Promise.resolve());
	const saveSeqRef = useRef(0);

	// Accept a persisted value only while the Host has proven it keeps the field.
	useEffect(() => {
		if (hostSupported !== true) return;
		setMount(normalizeBibtexMount(settings.bibtexMount));
	}, [settings.bibtexMount, hostSupported]);

	// biome-ignore lint/correctness/useExhaustiveDependencies: tagReload is a refresh signal
	useEffect(() => {
		if (!vaultPath || !hasHostTransport()) {
			setTags([]);
			setTagStatus("unavailable");
			return;
		}
		let cancelled = false;
		setTagStatus("loading");
		void readTagsIndex(vaultPath)
			.then((next) => {
				if (cancelled) return;
				setTags(next);
				setTagStatus("ready");
			})
			.catch(() => {
				if (cancelled) return;
				setTags([]);
				setTagStatus("unavailable");
			});
		return () => {
			cancelled = true;
		};
	}, [vaultPath, tagReload]);

	useEffect(() => {
		if (dirReload === 0) {
			const fromStore = collectPaperDirs(storeTree);
			if (fromStore) {
				setDirs(fromStore);
				setDirStatus("ready");
				return;
			}
		}
		if (!vaultPath || !hasHostTransport()) {
			setDirs([]);
			setDirStatus("unavailable");
			return;
		}
		let cancelled = false;
		setDirStatus("loading");
		void loadVaultTree(vaultPath)
			.then((tree) => {
				if (cancelled) return;
				setDirs(collectPaperDirs(tree) ?? []);
				setDirStatus("ready");
			})
			.catch(() => {
				if (cancelled) return;
				setDirs([]);
				setDirStatus("unavailable");
			});
		return () => {
			cancelled = true;
		};
	}, [storeTree, vaultPath, dirReload]);

	useEffect(() => {
		if (saveStatus !== "saved") return;
		const timer = setTimeout(() => setSaveStatus("idle"), 2500);
		return () => clearTimeout(timer);
	}, [saveStatus]);

	const persist = useCallback((next: BibtexMountSettings) => {
		setSaveStatus("saving");
		const seq = ++saveSeqRef.current;
		saveQueueRef.current = saveQueueRef.current.then(async () => {
			try {
				const saved = await saveSettingsAsync({
					...settingsRef.current,
					bibtexMount: next,
				});
				if (seq !== saveSeqRef.current) return;
				const persisted = normalizeBibtexMount(saved.bibtexMount);
				setHostSupported(sameBibtexMount(persisted, next));
				// Patch the parent even when the Host dropped the field: otherwise
				// the next unrelated settings save would write the old value.
				patchRef.current({ bibtexMount: next });
				setSaveStatus("saved");
			} catch (error) {
				if (seq !== saveSeqRef.current) return;
				setSaveStatus("error");
				notifyError(errorText(error));
			}
		});
	}, []);

	const applyMount = useCallback(
		(next: BibtexMountSettings) => {
			const normalized = normalizeBibtexMount(next);
			setMount(normalized);
			persist(normalized);
		},
		[persist],
	);

	/**
	 * Prefer the tag index casing over a stale persisted value so a checked
	 * row and the saved name stay the same string (the Host de-dups
	 * case-insensitively).
	 */
	const selectedTags = useMemo(() => {
		if (tags.length === 0) return mount.tags;
		const canonical = new Map(
			tags.map((tag) => [tag.name.toLowerCase(), tag.name]),
		);
		return mount.tags.map((tag) => canonical.get(tag.toLowerCase()) ?? tag);
	}, [mount.tags, tags]);

	const tagOptions = useMemo(() => {
		const known = tags.map((tag) => ({
			value: tag.name,
			label: tag.name,
			color: tag.color,
		}));
		const knownNames = new Set(known.map((option) => option.value));
		const missing = selectedTags
			.filter((tag) => !knownNames.has(tag))
			.map((tag) => ({ value: tag, label: tag }));
		return [...missing, ...known];
	}, [selectedTags, tags]);

	const dirOptions = useMemo(() => {
		const knownDirs = new Set(dirs);
		const missing = mount.dirs
			.filter((dir) => !knownDirs.has(dir))
			.map((dir) => ({ value: dir, label: dir }));
		return [...missing, ...dirs.map((dir) => ({ value: dir, label: dir }))];
	}, [dirs, mount.dirs]);

	const tagEmptyLabel =
		tagStatus === "loading"
			? t("experimental.bibtexMount.tags.loading")
			: tagStatus === "unavailable"
				? t("experimental.bibtexMount.tags.unavailable")
				: t("experimental.bibtexMount.tags.none");
	const tagDescription =
		tagStatus === "loading"
			? t("experimental.bibtexMount.tags.loading")
			: tagStatus === "unavailable"
				? t("experimental.bibtexMount.tags.unavailable")
				: tags.length === 0
					? t("experimental.bibtexMount.tags.none")
					: undefined;
	const dirEmptyLabel =
		dirStatus === "loading"
			? t("experimental.bibtexMount.dirs.loading")
			: dirStatus === "unavailable"
				? t("experimental.bibtexMount.dirs.unavailable")
				: t("experimental.bibtexMount.dirs.none");
	const dirDescription =
		dirStatus === "loading"
			? t("experimental.bibtexMount.dirs.loading")
			: dirStatus === "unavailable"
				? t("experimental.bibtexMount.dirs.unavailable")
				: dirs.length === 0
					? t("experimental.bibtexMount.dirs.none")
					: undefined;

	// One stable mount path for both bibliography flavours; the selected
	// format decides the serialized entry style.
	const mountPath = "/agentero_online.bib";
	const feedbackText =
		saveStatus === "saving"
			? t("experimental.bibtexMount.save.saving")
			: saveStatus === "saved"
				? t("experimental.bibtexMount.save.saved")
				: saveStatus === "error"
					? t("experimental.bibtexMount.save.failed")
					: null;
	const showFeedback = feedbackText !== null || hostSupported === false;

	return (
		<>
			<SettingsSectionLabel>
				{t("experimental.bibtexMount.section")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("experimental.bibtexMount.enable")}
					htmlFor="bibtex-mount-enabled"
				>
					{mount.enabled ? (
						<span className="text-muted-foreground text-xs" aria-live="polite">
							{t("experimental.bibtexMount.mountedAt", { path: mountPath })}
						</span>
					) : null}
					<Switch
						id="bibtex-mount-enabled"
						checked={mount.enabled}
						onCheckedChange={(enabled) => applyMount({ ...mount, enabled })}
					/>
				</SettingsRow>
				<SettingsRow
					label={t("experimental.bibtexMount.format")}
					htmlFor="bibtex-mount-format"
				>
					<Select
						value={mount.format}
						onValueChange={(format) =>
							applyMount({
								...mount,
								format: isBibtexMountFormat(format) ? format : mount.format,
							})
						}
					>
						<SelectTrigger
							id="bibtex-mount-format"
							size="sm"
							className="w-[140px]"
						>
							<SelectValue />
						</SelectTrigger>
						<SelectContent>
							<SelectItem value="bibtex">BibTeX</SelectItem>
							<SelectItem value="biblatex">BibLaTeX</SelectItem>
						</SelectContent>
					</Select>
				</SettingsRow>
			</SettingsGroup>

			<SettingsSectionLabel>
				{t("experimental.bibtexMount.scope")}
			</SettingsSectionLabel>
			<SettingsGroup>
				<SettingsRow
					label={t("experimental.bibtexMount.tags.label")}
					htmlFor="bibtex-mount-tags"
					description={tagDescription}
				>
					<MountMultiSelect
						id="bibtex-mount-tags"
						options={tagOptions}
						selected={selectedTags}
						onChange={(next) => applyMount({ ...mount, tags: next })}
						allLabel={t("experimental.bibtexMount.tags.all")}
						emptyLabel={tagEmptyLabel}
						searchPlaceholder={t("experimental.bibtexMount.tags.search")}
						refreshLabel={t("experimental.bibtexMount.refresh")}
						status={tagStatus}
						onRefresh={() => setTagReload((value) => value + 1)}
					/>
				</SettingsRow>
				<SettingsRow
					label={t("experimental.bibtexMount.dirs.label")}
					htmlFor="bibtex-mount-dirs"
					description={dirDescription}
				>
					<MountMultiSelect
						id="bibtex-mount-dirs"
						options={dirOptions}
						selected={mount.dirs}
						onChange={(next) => applyMount({ ...mount, dirs: next })}
						allLabel={t("experimental.bibtexMount.dirs.all")}
						emptyLabel={dirEmptyLabel}
						searchPlaceholder={t("experimental.bibtexMount.dirs.search")}
						refreshLabel={t("experimental.bibtexMount.refresh")}
						status={dirStatus}
						onRefresh={() => setDirReload((value) => value + 1)}
					/>
				</SettingsRow>
			</SettingsGroup>

			{showFeedback ? (
				<div className="mb-5 flex items-start gap-1.5 px-1 text-muted-foreground text-xs">
					{saveStatus === "saving" ? (
						<Loader2 className="mt-0.5 size-3.5 shrink-0 animate-spin" />
					) : saveStatus === "error" || hostSupported === false ? (
						<CircleAlert className="mt-0.5 size-3.5 shrink-0 text-amber-600 dark:text-amber-400" />
					) : (
						<Check className="mt-0.5 size-3.5 shrink-0 text-emerald-600 dark:text-emerald-400" />
					)}
					<span className={cn(saveStatus === "error" && "text-destructive")}>
						{feedbackText}
						{feedbackText && hostSupported === false ? " " : null}
						{hostSupported === false ? (
							<span className="text-amber-700 dark:text-amber-400">
								{t("experimental.bibtexMount.save.unsupported")}
							</span>
						) : null}
					</span>
				</div>
			) : null}
		</>
	);
}
