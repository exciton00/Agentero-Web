/**
 * WeKnora RAG integration — shared types (settings, RPC payloads, events).
 *
 * Mirrors the web-host contract:
 * - settings live under `AppSettings.weknora` (server stores the API key; the
 *   settings wire only ever echoes a mask back);
 * - RPC payloads match `crates/agentero-web/src/handlers/weknora.rs`;
 * - events `weknora:sync` / `weknora:changed` are typed here.
 */

/** Auto-sync interval default (minutes) — P1 sync engine spec. */
export const DEFAULT_WEKNORA_AUTO_SYNC_MINUTES = 30;

export type WeKnoraSyncSettings = {
	includePaperBody: boolean;
	includeNotes: boolean;
	includeMarks: boolean;
	includeFigureIndex: boolean;
	includeCitation: boolean;
	/** Queue body parsing for papers without PAPER.md before rendering. */
	ensurePaperBody: boolean;
	autoSync: boolean;
	autoSyncMinutes: number;
};

export type WeKnoraToolsSettings = {
	exposeToAgent: boolean;
	exposeToMcp: boolean;
};

export type WeKnoraRetrievalSettings = {
	rerank: boolean;
	timeoutMs: number;
	topK: number;
};

export type WeKnoraAdvancedSettings = {
	concurrency: number;
	indexTimeoutSecs: number;
};

/** Persisted settings (`AppSettings.weknora`, masked API key on read). */
export type WeKnoraSettings = {
	enabled: boolean;
	baseUrl: string;
	/** Masked (`***`) when echoed back from the Host; write-only in the UI. */
	apiKey: string;
	knowledgeBaseId: string;
	sync: WeKnoraSyncSettings;
	tools: WeKnoraToolsSettings;
	retrieval: WeKnoraRetrievalSettings;
	advanced: WeKnoraAdvancedSettings;
};

/** Fresh defaults (deep copy — safe to mutate). */
export function defaultWeKnoraSettings(): WeKnoraSettings {
	return {
		enabled: false,
		baseUrl: "",
		apiKey: "",
		knowledgeBaseId: "",
		sync: {
			includePaperBody: true,
			includeNotes: true,
			includeMarks: true,
			includeFigureIndex: true,
			includeCitation: true,
			ensurePaperBody: true,
			autoSync: false,
			autoSyncMinutes: DEFAULT_WEKNORA_AUTO_SYNC_MINUTES,
		},
		tools: {
			exposeToAgent: true,
			exposeToMcp: true,
		},
		retrieval: {
			rerank: false,
			timeoutMs: 15_000,
			topK: 8,
		},
		advanced: {
			concurrency: 2,
			indexTimeoutSecs: 180,
		},
	};
}

function asRecord(value: unknown): Record<string, unknown> {
	return typeof value === "object" && value !== null
		? (value as Record<string, unknown>)
		: {};
}

function boolOr(value: unknown, fallback: boolean): boolean {
	return typeof value === "boolean" ? value : fallback;
}

function stringOr(value: unknown, fallback: string): string {
	return typeof value === "string" ? value : fallback;
}

function intOr(value: unknown, fallback: number, min: number, max: number) {
	if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
	return Math.min(max, Math.max(min, Math.round(value)));
}

/**
 * Validate an arbitrary settings snapshot (persisted file, Host wire, or a
 * hand-edited object) into a complete {@link WeKnoraSettings}. The API key is
 * deliberately passed through untouched: a mask must survive so the Host can
 * keep the stored secret on write.
 */
export function normalizeWeKnoraSettings(raw: unknown): WeKnoraSettings {
	const base = defaultWeKnoraSettings();
	const root = asRecord(raw);
	const sync = asRecord(root.sync);
	const tools = asRecord(root.tools);
	const retrieval = asRecord(root.retrieval);
	const advanced = asRecord(root.advanced);
	return {
		enabled: boolOr(root.enabled, base.enabled),
		baseUrl: stringOr(root.baseUrl, base.baseUrl).trim(),
		apiKey: stringOr(root.apiKey, base.apiKey).trim(),
		knowledgeBaseId: stringOr(
			root.knowledgeBaseId,
			base.knowledgeBaseId,
		).trim(),
		sync: {
			includePaperBody: boolOr(
				sync.includePaperBody,
				base.sync.includePaperBody,
			),
			includeNotes: boolOr(sync.includeNotes, base.sync.includeNotes),
			includeMarks: boolOr(sync.includeMarks, base.sync.includeMarks),
			includeFigureIndex: boolOr(
				sync.includeFigureIndex,
				base.sync.includeFigureIndex,
			),
			includeCitation: boolOr(sync.includeCitation, base.sync.includeCitation),
			ensurePaperBody: boolOr(sync.ensurePaperBody, base.sync.ensurePaperBody),
			autoSync: boolOr(sync.autoSync, base.sync.autoSync),
			autoSyncMinutes: intOr(
				sync.autoSyncMinutes,
				base.sync.autoSyncMinutes,
				1,
				24 * 60,
			),
		},
		tools: {
			exposeToAgent: boolOr(tools.exposeToAgent, base.tools.exposeToAgent),
			exposeToMcp: boolOr(tools.exposeToMcp, base.tools.exposeToMcp),
		},
		retrieval: {
			rerank: boolOr(retrieval.rerank, base.retrieval.rerank),
			timeoutMs: intOr(
				retrieval.timeoutMs,
				base.retrieval.timeoutMs,
				1_000,
				120_000,
			),
			topK: intOr(retrieval.topK, base.retrieval.topK, 1, 50),
		},
		advanced: {
			concurrency: intOr(advanced.concurrency, base.advanced.concurrency, 1, 4),
			indexTimeoutSecs: intOr(
				advanced.indexTimeoutSecs,
				base.advanced.indexTimeoutSecs,
				10,
				3_600,
			),
		},
	};
}

/** `true` when the Host echoed a masked key (a real secret is stored). */
export function hasStoredWeKnoraApiKey(apiKey: string): boolean {
	const value = apiKey.trim();
	return value.length > 0 && /^\*+$/.test(value);
}

/* -------------------------------------------------------------------------- */
/* RPC payloads                                                               */
/* -------------------------------------------------------------------------- */

export type WeKnoraSyncPhase = "scan" | "render" | "sync" | "index" | "done";

export const WEKNORA_SYNC_PHASES: readonly WeKnoraSyncPhase[] = [
	"scan",
	"render",
	"sync",
	"index",
	"done",
] as const;

export function isWeKnoraSyncPhase(value: unknown): value is WeKnoraSyncPhase {
	return (
		typeof value === "string" &&
		(WEKNORA_SYNC_PHASES as readonly string[]).includes(value)
	);
}

export type WeKnoraSyncError = {
	paper: string;
	error: string;
};

/**
 * Progress snapshot carried inside `weknora_status.sync.progress` (and as the
 * common part of the `weknora:sync` event). Only the positional fields are
 * persisted; per-run counters/errors live on the event.
 */
export type WeKnoraStatusProgress = {
	phase: WeKnoraSyncPhase;
	done: number;
	total: number;
	current?: string | null;
};

/** `weknora:sync` event payload (live progress + final counters). */
export type WeKnoraSyncProgress = WeKnoraStatusProgress & {
	created: number;
	updated: number;
	skipped: number;
	deleted: number;
	failed: number;
	errors: WeKnoraSyncError[];
	running: boolean;
};

export type WeKnoraSyncStats = {
	total: number;
	synced: number;
	indexing: number;
	failed: number;
	pending: number;
	lastSyncAt?: string | null;
	lastError?: string | null;
	running: boolean;
	progress?: WeKnoraStatusProgress | null;
};

export type WeKnoraStatus = {
	enabled: boolean;
	configured: boolean;
	baseUrl: string;
	knowledgeBaseId: string;
	kbName?: string | null;
	sync: WeKnoraSyncStats;
	autoSync: { enabled: boolean; intervalMinutes: number };
	tools: { agent: boolean; mcp: boolean };
};

export type WeKnoraTestResult = {
	ok: boolean;
	kbName?: string | null;
	knowledgeCount?: number | null;
	embeddingModelId?: string | null;
	error?: string | null;
};

export type WeKnoraKnowledgeBase = {
	id: string;
	name: string;
	knowledgeCount: number;
	description?: string | null;
	capabilities?: string[] | null;
};

export type WeKnoraListKbsResult = {
	kbs: WeKnoraKnowledgeBase[];
	error?: string | null;
};

export type WeKnoraSyncStartArgs = {
	force?: boolean;
	/** Restrict the run to one paper (id or vault-relative path). */
	paper?: string;
};

export type WeKnoraSyncStartResult = {
	started: boolean;
	total: number;
	changed: number;
	skipped: number;
	deleted: number;
};

export type WeKnoraSyncCancelResult = {
	cancelled: boolean;
};

export type WeKnoraReconcileArgs = {
	prune?: boolean;
};

export type WeKnoraReconcileResult = {
	adopted: number;
	missing: number;
	orphans: number;
	deleted: number;
};

export type WeKnoraClearMappingArgs = {
	deleteDocs?: boolean;
};

export type WeKnoraClearMappingResult = {
	cleared: number;
	deleted: number;
};

/** `weknora:changed` payload — the same shape as {@link WeKnoraStatus}. */
export type WeKnoraStatusEvent = WeKnoraStatus;
