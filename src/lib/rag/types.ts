/**
 * RAG integration — shared types (settings, RPC payloads, events).
 *
 * v2.1 provider abstraction: WeKnora is the first supported external knowledge
 * base; RAGFlow is reserved for a future provider. The contract lives at the
 * RAG layer, so the wire/RPC names are generic (`rag_*`, `rag:sync`).
 *
 * - settings live under `AppSettings.rag` (server stores the API key; the wire
 *   only ever echoes a mask back; legacy `weknora` settings are migrated);
 * - RPC payloads match `crates/agentero-web/src/handlers/rag.rs`;
 * - events `rag:sync` / `rag:changed` are typed here.
 */

/** Auto-sync interval default (minutes) — sync engine spec. */
export const DEFAULT_RAG_AUTO_SYNC_MINUTES = 30;

/** Supported RAG providers. `ragflow` is reserved (not implemented yet). */
export const RAG_PROVIDER_IDS = ["weknora", "ragflow"] as const;
export type RagProviderId = (typeof RAG_PROVIDER_IDS)[number];

export const DEFAULT_RAG_PROVIDER: RagProviderId = "weknora";

export function isRagProviderId(value: unknown): value is RagProviderId {
	return (
		typeof value === "string" &&
		(RAG_PROVIDER_IDS as readonly string[]).includes(value)
	);
}

/** Provider capabilities echoed by `rag_status`. */
export type RagCapabilities = {
	manualMarkdown: boolean;
	inPlaceUpdate: boolean;
	tags: boolean;
	customMetadata: boolean;
};

/** UI choice list: only `weknora` is selectable in v2.1. */
export type RagProviderChoice = {
	id: RagProviderId;
	label: string;
	available: boolean;
};

export const RAG_PROVIDER_CHOICES: readonly RagProviderChoice[] = [
	{ id: "weknora", label: "WeKnora", available: true },
	{ id: "ragflow", label: "RAGFlow", available: false },
] as const;

export type RagSyncSettings = {
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

export type RagToolsSettings = {
	exposeToAgent: boolean;
	exposeToMcp: boolean;
};

export type RagRetrievalSettings = {
	rerank: boolean;
	timeoutMs: number;
	topK: number;
};

export type RagAdvancedSettings = {
	concurrency: number;
	indexTimeoutSecs: number;
};

/** Persisted settings (`AppSettings.rag`, masked API key on read). */
export type RagSettings = {
	/** External knowledge-base provider (`weknora` for v2.1). */
	provider: RagProviderId;
	enabled: boolean;
	baseUrl: string;
	/** Masked (`***`) when echoed back from the Host; write-only in the UI. */
	apiKey: string;
	knowledgeBaseId: string;
	sync: RagSyncSettings;
	tools: RagToolsSettings;
	retrieval: RagRetrievalSettings;
	advanced: RagAdvancedSettings;
};

/** Fresh defaults (deep copy — safe to mutate). */
export function defaultRagSettings(): RagSettings {
	return {
		provider: DEFAULT_RAG_PROVIDER,
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
			autoSyncMinutes: DEFAULT_RAG_AUTO_SYNC_MINUTES,
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

export function providerLabel(
	provider: RagProviderId | string | null | undefined,
): string {
	const choice = RAG_PROVIDER_CHOICES.find((item) => item.id === provider);
	return choice?.label ?? "RAG";
}

/**
 * Validate an arbitrary settings snapshot (persisted file, Host wire, or a
 * hand-edited object) into a complete {@link RagSettings}. The API key is
 * deliberately passed through untouched: a mask must survive so the Host can
 * keep the stored secret on write.
 */
export function normalizeRagSettings(raw: unknown): RagSettings {
	const base = defaultRagSettings();
	const root = asRecord(raw);
	const sync = asRecord(root.sync);
	const tools = asRecord(root.tools);
	const retrieval = asRecord(root.retrieval);
	const advanced = asRecord(root.advanced);
	return {
		provider: isRagProviderId(root.provider) ? root.provider : base.provider,
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
export function hasStoredRagApiKey(apiKey: string): boolean {
	const value = apiKey.trim();
	return value.length > 0 && /^\*+$/.test(value);
}

/* -------------------------------------------------------------------------- */
/* RPC payloads                                                               */
/* -------------------------------------------------------------------------- */

export type RagSyncPhase = "scan" | "render" | "sync" | "index" | "done";

export const RAG_SYNC_PHASES: readonly RagSyncPhase[] = [
	"scan",
	"render",
	"sync",
	"index",
	"done",
] as const;

export function isRagSyncPhase(value: unknown): value is RagSyncPhase {
	return (
		typeof value === "string" &&
		(RAG_SYNC_PHASES as readonly string[]).includes(value)
	);
}

export type RagSyncError = {
	paper: string;
	error: string;
};

/**
 * Progress snapshot carried inside `rag_status.sync.progress` (and as the
 * common part of the `rag:sync` event). Only the positional fields are
 * persisted; per-run counters/errors live on the event.
 */
export type RagStatusProgress = {
	phase: RagSyncPhase;
	done: number;
	total: number;
	current?: string | null;
};

/** `rag:sync` event payload (live progress + final counters). */
export type RagSyncProgress = RagStatusProgress & {
	created: number;
	updated: number;
	skipped: number;
	deleted: number;
	failed: number;
	errors: RagSyncError[];
	running: boolean;
};

export type RagSyncStats = {
	total: number;
	synced: number;
	indexing: number;
	failed: number;
	pending: number;
	lastSyncAt?: string | null;
	lastError?: string | null;
	running: boolean;
	progress?: RagStatusProgress | null;
};

export type RagStatus = {
	/** Active provider id (bare string, e.g. `weknora`). */
	provider?: RagProviderId | string | null;
	/** Human-readable provider name (e.g. `WeKnora`). */
	providerLabel?: string | null;
	/** False for reserved providers (e.g. `ragflow`). */
	providerImplemented?: boolean;
	capabilities?: RagCapabilities | null;
	enabled: boolean;
	configured: boolean;
	baseUrl: string;
	knowledgeBaseId: string;
	kbName?: string | null;
	sync: RagSyncStats;
	autoSync: { enabled: boolean; intervalMinutes: number };
	tools: { agent: boolean; mcp: boolean };
};

export type RagTestResult = {
	provider?: RagProviderId | string | null;
	providerLabel?: string | null;
	ok: boolean;
	kbName?: string | null;
	knowledgeCount?: number | null;
	embeddingModelId?: string | null;
	error?: string | null;
};

export type RagKnowledgeBase = {
	provider?: RagProviderId | string | null;
	providerLabel?: string | null;
	id: string;
	name: string;
	knowledgeCount: number;
	description?: string | null;
	capabilities?: string[] | null;
};

export type RagListKbsResult = {
	provider?: RagProviderId | string | null;
	providerLabel?: string | null;
	kbs: RagKnowledgeBase[];
	error?: string | null;
};

export type RagSyncStartArgs = {
	force?: boolean;
	/** Restrict the run to one paper (id or vault-relative path). */
	paper?: string;
};

export type RagSyncStartResult = {
	started: boolean;
	total: number;
	changed: number;
	skipped: number;
	deleted: number;
};

export type RagSyncCancelResult = {
	cancelled: boolean;
};

export type RagReconcileArgs = {
	prune?: boolean;
};

export type RagReconcileResult = {
	adopted: number;
	missing: number;
	orphans: number;
	deleted: number;
};

export type RagClearMappingArgs = {
	deleteDocs?: boolean;
};

export type RagClearMappingResult = {
	cleared: number;
	deleted: number;
};

/** `rag:changed` payload — the same shape as {@link RagStatus}. */
export type RagStatusEvent = RagStatus;

/* -------------------------------------------------------------------------- */
/* Search (exposed to MCP/agent; the settings pane does not render it yet)     */
/* -------------------------------------------------------------------------- */

export type RagSearchMode = "hybrid" | "semantic" | "keyword";

export type RagSearchArgs = {
	query: string;
	topK?: number;
	paper?: string;
	tag?: string;
	mode?: RagSearchMode;
	rerank?: boolean;
	includeSummaries?: boolean;
};

export type RagSearchHit = {
	provider?: RagProviderId | string | null;
	paper: {
		ref: string;
		path: string;
		title?: string | null;
		tags?: string[] | null;
		bibtexKey?: string | null;
	};
	knowledgeId: string;
	chunkType?: string | null;
	score: number;
	content: string;
	section?: string | null;
	latex?: string | null;
};

export type RagSearchResult = {
	provider?: RagProviderId | string | null;
	providerLabel?: string | null;
	query: string;
	hits: RagSearchHit[];
	count: number;
	degraded?: boolean;
};
