/**
 * Agentero-side cache of Agent chat transcripts (`ChatLine[]`).
 *
 * Why this exists: Dsh can end a turn `interrupted` without ever persisting the
 * final assistant text — the durable log then only replays reasoning/tool
 * parts. The user still watched the answer stream into the panel, so the only
 * remaining copy lives in the browser. Saving the visible transcript lets
 * history loading fill that missing text after a refresh.
 *
 * Authority rules (disk/ACP always wins):
 * - a cached agent line only supplies text for durable agent lines that have no
 *   visible text (durable reasoning/tools/order stay untouched);
 * - cached turns that the durable transcript lost completely are appended;
 * - user/notice/error/system lines from the durable transcript are preserved.
 *
 * Bounded + best-effort: versioned localStorage key, capped lines/line size/
 * session size/entry count/total size, images & base64 attachments stripped,
 * corrupt JSON or unavailable storage reads as "no cache". Never writes into
 * the Vault / catalog.
 */

import {
	type AgentPart,
	agentTextFromParts,
	type ChatLine,
	failIncompleteTools,
	mapToolStatus,
	type ToolUiState,
} from "@/lib/agent/chat-state";
import { stripPromptEnvelopeForDisplay } from "@/lib/agent/prompt-display";
import {
	readJsonStorage,
	removeStorageKey,
	writeJsonStorage,
} from "@/lib/core/storage";

export const TRANSCRIPT_CACHE_STORAGE_KEY = "agentero.agent-transcripts.v1";
const CACHE_VERSION = 1;

/** Trailing-edge debounce for transcript saves. */
export const TRANSCRIPT_SAVE_DEBOUNCE_MS = 500;
/** Hard upper bound between saves during continuous streaming. */
const MAX_SAVE_DELAY_MS = 2_000;

/** Newest lines kept per session (turn-aligned: never starts mid-turn). */
export const MAX_CACHED_LINES = 100;
/** Per-line character cap (≈200 KB of mostly-ASCII text). */
export const MAX_LINE_CHARS = 200_000;
/** Per-session serialized character cap. */
export const MAX_SESSION_CHARS = 1_000_000;
/** Newest sessions kept in the cache. */
export const MAX_CACHE_ENTRIES = 40;
/** Serialized size cap for the whole cache (localStorage quota safety). */
export const MAX_TOTAL_CHARS = 2_000_000;

/** Tool input/output payloads above this size are dropped (screenshots/base64). */
const MAX_TOOL_PAYLOAD_CHARS = 4_000;
const MAX_PARTS_PER_LINE = 200;
const MAX_SOURCES = 20;
const MAX_SOURCE_CHARS = 2_000;
const MAX_PLAN_ENTRIES = 30;
const MAX_ID_CHARS = 256;

type AgentChatLine = Extract<ChatLine, { kind: "agent" }>;
type UserChatLine = Extract<ChatLine, { kind: "user" }>;

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isAgentChatLine(line: ChatLine): line is AgentChatLine {
	return line.kind === "agent";
}

export function isUserChatLine(line: ChatLine): line is UserChatLine {
	return line.kind === "user";
}

/** True when any agent line carries non-whitespace assistant text. */
export function hasVisibleAssistantText(lines: ChatLine[]): boolean {
	return lines.some(
		(line) =>
			line.kind === "agent" && agentTextFromParts(line.parts).trim().length > 0,
	);
}

function cleanId(value: string): string {
	// The cache key joins two ids with a NUL separator; never allow one inside.
	const trimmed = value.trim().replaceAll("\u0000", "").slice(0, MAX_ID_CHARS);
	return trimmed;
}

/** localStorage key field for one agent conversation. */
export function transcriptCacheKey(agentId: string, sessionId: string): string {
	return `${cleanId(agentId)}\u0000${cleanId(sessionId)}`;
}

function entryKeyOfAgent(agentId: string): string {
	return `${cleanId(agentId)}\u0000`;
}

function clampText(text: string, max = MAX_LINE_CHARS): string {
	return text.length > max ? text.slice(0, max) : text;
}

// ---------------------------------------------------------------------------
// Sanitize (write path)
// ---------------------------------------------------------------------------

/** JSON-serializable length, or Infinity for values that cannot be stored. */
function serializedLength(value: unknown): number {
	try {
		const json = JSON.stringify(value);
		return json === undefined ? Number.POSITIVE_INFINITY : json.length;
	} catch {
		return Number.POSITIVE_INFINITY;
	}
}

/** Keep small tool payloads; drop screenshots/base64 blobs. */
function capToolPayload(value: unknown): unknown {
	if (value === undefined) return undefined;
	return serializedLength(value) > MAX_TOOL_PAYLOAD_CHARS ? undefined : value;
}

function normalizeToolInput(raw: unknown): ToolUiState | null {
	if (!isRecord(raw)) return null;
	const id = typeof raw.id === "string" ? raw.id.slice(0, MAX_ID_CHARS) : "";
	if (!id) return null;
	const tool: ToolUiState = {
		id,
		title:
			typeof raw.title === "string" ? raw.title.slice(0, MAX_SOURCE_CHARS) : "",
		kind:
			typeof raw.kind === "string" ? raw.kind.slice(0, MAX_ID_CHARS) : "other",
		status: mapToolStatus(typeof raw.status === "string" ? raw.status : null),
	};
	const input = capToolPayload(raw.input);
	if (input !== undefined) tool.input = input;
	const output = capToolPayload(raw.output);
	if (output !== undefined) tool.output = output;
	return tool;
}

function sanitizePlanEntries(raw: unknown): Array<{
	content: string;
	status: string;
	priority: string;
}> {
	if (!Array.isArray(raw)) return [];
	const entries: Array<{ content: string; status: string; priority: string }> =
		[];
	for (const value of raw.slice(0, MAX_PLAN_ENTRIES)) {
		if (!isRecord(value)) continue;
		if (typeof value.content !== "string" || !value.content.trim()) continue;
		entries.push({
			content: value.content.slice(0, MAX_SOURCE_CHARS),
			status:
				typeof value.status === "string"
					? value.status.slice(0, MAX_ID_CHARS)
					: "",
			priority:
				typeof value.priority === "string"
					? value.priority.slice(0, MAX_ID_CHARS)
					: "",
		});
	}
	return entries;
}

function sanitizeParts(raw: unknown): AgentPart[] {
	if (!Array.isArray(raw)) return [];
	const parts: AgentPart[] = [];
	for (
		let index = 0;
		index < raw.length && parts.length < MAX_PARTS_PER_LINE;
		index++
	) {
		const part = raw[index];
		if (!isRecord(part)) continue;
		const id =
			typeof part.id === "string"
				? part.id.slice(0, MAX_ID_CHARS)
				: `part-${index}`;
		if (part.type === "text" || part.type === "reasoning") {
			if (typeof part.text !== "string" || !part.text) continue;
			parts.push({ type: part.type, id, text: clampText(part.text) });
			continue;
		}
		if (part.type === "tool") {
			const tool = normalizeToolInput(part.tool);
			if (tool) parts.push({ type: "tool", id, tool });
			continue;
		}
		if (part.type === "plan") {
			const entries = sanitizePlanEntries(part.entries);
			if (entries.length) parts.push({ type: "plan", id, entries });
		}
	}
	return parts;
}

/** Shrink an over-budget agent line by truncating its longest text parts. */
function clampPartsToLineBudget(parts: AgentPart[]): AgentPart[] | null {
	let current = parts;
	for (let guard = 0; guard < MAX_PARTS_PER_LINE; guard++) {
		const size = serializedLength(current);
		if (size <= MAX_LINE_CHARS) return current;
		const overflow = size - MAX_LINE_CHARS;
		let longest = -1;
		let longestLength = 0;
		for (let index = 0; index < current.length; index++) {
			const part = current[index];
			if (part.type !== "text" && part.type !== "reasoning") continue;
			if (part.text.length > longestLength) {
				longest = index;
				longestLength = part.text.length;
			}
		}
		if (longest < 0) return null;
		const target = current[longest];
		if (target.type !== "text" && target.type !== "reasoning") return null;
		const keep = Math.max(0, target.text.length - overflow - 64);
		if (keep === 0) {
			current = current.filter((_, index) => index !== longest);
		} else {
			const next = current.slice();
			next[longest] = { ...target, text: target.text.slice(0, keep) };
			current = next;
		}
	}
	return null;
}

function sanitizeSources(raw: unknown): string[] {
	if (!Array.isArray(raw)) return [];
	const sources: string[] = [];
	for (const value of raw.slice(0, MAX_SOURCES)) {
		if (typeof value !== "string") continue;
		const source = value.slice(0, MAX_SOURCE_CHARS);
		if (source) sources.push(source);
	}
	return sources;
}

/**
 * Reduce one visible `ChatLine` to a bounded, JSON-safe cache line.
 *
 * User attachments (composer images, PDF visual crops, selection chips) are
 * dropped: they are large base64 payloads and are already session-local (never
 * replayed by ACP history).
 */
export function sanitizeTranscriptLine(
	line: unknown,
	index = 0,
): ChatLine | null {
	if (!isRecord(line)) return null;
	const kind = line.kind;
	if (
		kind !== "user" &&
		kind !== "agent" &&
		kind !== "error" &&
		kind !== "system" &&
		kind !== "notice"
	) {
		return null;
	}
	const id =
		typeof line.id === "string" && line.id
			? line.id.slice(0, MAX_ID_CHARS)
			: `cached-${index}`;
	if (kind === "agent") {
		const parts = clampPartsToLineBudget(sanitizeParts(line.parts));
		if (!parts || parts.length === 0) return null;
		const sources = sanitizeSources(line.sources);
		return {
			id,
			kind,
			parts,
			streaming: false,
			...(sources.length ? { sources } : {}),
		};
	}
	if (kind === "user") {
		return {
			id,
			kind,
			text: typeof line.text === "string" ? clampText(line.text) : "",
		};
	}
	return {
		id,
		kind,
		text: typeof line.text === "string" ? clampText(line.text) : "",
	};
}

/** Drop the oldest lines while keeping the newest turn-aligned window. */
function trimLinesToBudget(lines: ChatLine[]): ChatLine[] {
	if (lines.length <= MAX_CACHED_LINES) return lines;
	let start = lines.length - MAX_CACHED_LINES;
	// Never start mid-turn: move forward to the next user line.
	while (start < lines.length - 1 && lines[start]?.kind !== "user") start++;
	return lines.slice(start);
}

/** Drop the oldest complete turns until the session fits its size budget. */
function trimSessionToBudget(lines: ChatLine[]): ChatLine[] {
	let current = lines;
	while (current.length > 1 && serializedLength(current) > MAX_SESSION_CHARS) {
		let start = 1;
		while (start < current.length - 1 && current[start]?.kind !== "user")
			start++;
		current = current.slice(start);
	}
	return current;
}

/** Bounded cache representation of a transcript (or a subset when long). */
export function sanitizeTranscriptLinesForCache(lines: ChatLine[]): ChatLine[] {
	if (!Array.isArray(lines) || lines.length === 0) return [];
	const sanitized: ChatLine[] = [];
	for (let index = 0; index < lines.length; index++) {
		const line = sanitizeTranscriptLine(lines[index], index);
		if (line) sanitized.push(line);
	}
	return trimSessionToBudget(trimLinesToBudget(sanitized));
}

// ---------------------------------------------------------------------------
// Normalize (read path — tolerate corrupt / hand-edited localStorage)
// ---------------------------------------------------------------------------

function normalizeCachedLine(raw: unknown, index: number): ChatLine | null {
	// Reuse the write sanitizer: it validates shapes and bounds every field.
	const line = sanitizeTranscriptLine(raw, index);
	if (!line) return null;
	if (line.kind !== "agent") return line;
	// Restored lines are historical; never resurrect a streaming indicator or
	// a spinner on an unfinished tool call.
	return { ...line, streaming: false, parts: failIncompleteTools(line.parts) };
}

/** Validate + bound a raw cached `lines` array (bad entries are skipped). */
export function normalizeCachedLines(raw: unknown): ChatLine[] {
	if (!Array.isArray(raw)) return [];
	const lines: ChatLine[] = [];
	// Corrupt data cannot exceed the sanitizer's own limits, but do not iterate
	// an unbounded array from a tampered storage value either.
	for (
		let index = 0;
		index < raw.length && index < MAX_CACHED_LINES * 4;
		index++
	) {
		const line = normalizeCachedLine(raw[index], index);
		if (line) lines.push(line);
	}
	return trimSessionToBudget(trimLinesToBudget(lines));
}

/** Force restored agent lines out of the live/streaming state. */
function normalizeRestoredLines(lines: ChatLine[]): ChatLine[] {
	return lines.map((line) => {
		if (line.kind !== "agent") return line;
		return {
			...line,
			streaming: false,
			parts: failIncompleteTools(line.parts),
		};
	});
}

// ---------------------------------------------------------------------------
// Storage read / write
// ---------------------------------------------------------------------------

function readRawEntries(): Record<string, unknown> {
	const parsed = readJsonStorage<unknown>(TRANSCRIPT_CACHE_STORAGE_KEY, null);
	if (!isRecord(parsed) || parsed.v !== CACHE_VERSION) return {};
	const entries = parsed.entries;
	if (!isRecord(entries)) return {};
	return entries;
}

function entryTimestamp(entry: unknown): number {
	if (!isRecord(entry)) return 0;
	const at = entry.at;
	return typeof at === "number" && Number.isFinite(at) ? at : 0;
}

/**
 * Timestamp for the entry being written. Strictly greater than every existing
 * entry so two saves in the same millisecond still prune deterministically
 * (and the newest conversation is never evicted first).
 */
function nextEntryTimestamp(entries: Record<string, unknown>): number {
	let newest = 0;
	for (const entry of Object.values(entries)) {
		newest = Math.max(newest, entryTimestamp(entry));
	}
	return Math.max(Date.now(), newest + 1);
}

function readEntryLines(entry: unknown): ChatLine[] | null {
	if (!isRecord(entry)) return null;
	const lines = normalizeCachedLines(entry.lines);
	return lines.length > 0 ? lines : null;
}

/** Trim raw entries to the newest count / total-size budget (no re-validation). */
function pruneRawEntries(
	entries: Record<string, unknown>,
	protectedKey: string,
): Record<string, unknown> {
	const ordered = Object.keys(entries).sort(
		(a, b) => entryTimestamp(entries[b]) - entryTimestamp(entries[a]),
	);
	const kept: Record<string, unknown> = {};
	let total = "{}".length;
	for (const key of ordered) {
		const entry = entries[key];
		if (entry === undefined) continue;
		const size = serializedLength(entry);
		const tooMany = Object.keys(kept).length >= MAX_CACHE_ENTRIES;
		// Always keep the entry being written, even when the cache is full.
		if (tooMany && key !== protectedKey) continue;
		const tooLarge = total + size > MAX_TOTAL_CHARS && key !== protectedKey;
		if (tooLarge) continue;
		kept[key] = entry;
		total += size;
	}
	return kept;
}

function writeCacheFile(entries: Record<string, unknown>): boolean {
	return writeJsonStorage(TRANSCRIPT_CACHE_STORAGE_KEY, {
		v: CACHE_VERSION,
		entries,
	});
}

/** Drop half of the oldest entries and retry once after a quota failure. */
function retryAfterQuotaFailure(
	entries: Record<string, unknown>,
	protectedKey: string,
): boolean {
	const ordered = Object.keys(entries).sort(
		(a, b) => entryTimestamp(entries[b]) - entryTimestamp(entries[a]),
	);
	const keepCount = Math.max(2, Math.ceil(ordered.length / 2));
	const retry: Record<string, unknown> = {};
	if (protectedKey && entries[protectedKey] !== undefined) {
		retry[protectedKey] = entries[protectedKey];
	}
	for (const key of ordered) {
		if (Object.keys(retry).length >= keepCount) break;
		if (retry[key] !== undefined) continue;
		retry[key] = entries[key];
	}
	return writeCacheFile(retry);
}

/**
 * Persist one conversation transcript. Returns false when nothing was written
 * (empty transcript, cleared entry, unavailable storage or quota failure).
 */
export function saveCachedTranscript(
	agentId: string,
	sessionId: string,
	lines: ChatLine[],
): boolean {
	const agent = cleanId(agentId);
	const session = cleanId(sessionId);
	if (!agent || !session) return false;
	const sanitized = sanitizeTranscriptLinesForCache(lines);
	if (sanitized.length === 0) {
		removeCachedTranscript(agent, session);
		return false;
	}
	const key = transcriptCacheKey(agent, session);
	const entries = readRawEntries();
	entries[key] = { at: nextEntryTimestamp(entries), lines: sanitized };
	const pruned = pruneRawEntries(entries, key);
	const written = writeCacheFile(pruned);
	if (written) return true;
	if (Object.keys(pruned).length <= 1) return false;
	return retryAfterQuotaFailure(pruned, key);
}

/** Exact-key lookup. Returns null for missing / corrupt / empty entries. */
export function loadCachedTranscript(
	agentId: string,
	sessionIds: Array<string | null | undefined>,
): ChatLine[] | null {
	const agent = cleanId(agentId);
	if (!agent) return null;
	const raw = readRawEntries();
	const tried = new Set<string>();
	for (const sessionId of sessionIds) {
		const session = cleanId(typeof sessionId === "string" ? sessionId : "");
		if (!session || tried.has(session)) continue;
		tried.add(session);
		const lines = readEntryLines(raw[transcriptCacheKey(agent, session)]);
		if (lines) return lines;
	}
	return null;
}

function userTurnText(line: ChatLine): string {
	if (line.kind !== "user") return "";
	return stripPromptEnvelopeForDisplay(line.text).trim();
}

function cachedUserTexts(lines: ChatLine[]): string[] {
	return lines
		.filter(isUserChatLine)
		.map(userTurnText)
		.filter((text) => text.length > 0);
}

/** Longest common prefix/suffix overlap between two user-turn sequences. */
function userTurnOverlapScore(
	reference: string[],
	candidate: string[],
): number {
	let prefix = 0;
	while (
		prefix < reference.length &&
		prefix < candidate.length &&
		reference[prefix] === candidate[prefix]
	) {
		prefix++;
	}
	let suffix = 0;
	while (
		suffix < reference.length &&
		suffix < candidate.length &&
		reference[reference.length - 1 - suffix] ===
			candidate[candidate.length - 1 - suffix]
	) {
		suffix++;
	}
	// Compare the visible user turns as text; a single shared turn is already
	// a signal when the session was interrupted before the provider persisted
	// anything else.
	return Math.max(prefix, suffix);
}

/**
 * Find a cached transcript whose user turns overlap the reference transcript.
 *
 * A turn that dies before `agent:completed` never learns its ACP provider
 * session id, so the cache is keyed by the Agentero runtime id while history
 * rows use the provider id. Matching the visible user turns bridges that gap.
 * Only called after an exact-key miss.
 *
 * Ambiguous matches (two different conversations sharing the same overlapping
 * user prompt, e.g. two threads both opened with "继续") are refused rather
 * than risking showing another conversation's answer.
 */
export function findCachedTranscriptByUserTurns(
	agentId: string,
	referenceLines: ChatLine[],
): ChatLine[] | null {
	const agent = cleanId(agentId);
	const reference = cachedUserTexts(referenceLines);
	if (!agent || reference.length === 0) return null;
	const prefix = entryKeyOfAgent(agent);
	let best: {
		lines: ChatLine[];
		score: number;
		at: number;
		json: string;
	} | null = null;
	let ambiguous = false;
	for (const [key, value] of Object.entries(readRawEntries())) {
		if (!key.startsWith(prefix)) continue;
		if (!isRecord(value)) continue;
		const lines = readEntryLines(value);
		if (!lines || !hasVisibleAssistantText(lines)) continue;
		const score = userTurnOverlapScore(reference, cachedUserTexts(lines));
		if (score === 0) continue;
		const json = JSON.stringify(lines);
		if (!best || score > best.score) {
			best = { lines, score, at: entryTimestamp(value), json };
			ambiguous = false;
			continue;
		}
		if (score !== best.score) continue;
		// Duplicate entries of the same conversation (runtime + provider key)
		// are not ambiguous; a different transcript with the same score is.
		if (json !== best.json) ambiguous = true;
		else if (entryTimestamp(value) > best.at) best.at = entryTimestamp(value);
	}
	if (!best || ambiguous) return null;
	return best.lines;
}

/**
 * Resolve the cached transcript for a history row: exact provider/runtime keys
 * first, then a user-turn match for conversations whose provider id was only
 * learned after the cache entry was written (interrupted first turn).
 *
 * An exact entry that has no assistant text (e.g. only a user turn was cached
 * before the answer arrived) does not block the user-turn match: another entry
 * of the same live conversation may hold the answer under the runtime id.
 */
export function resolveCachedTranscript(input: {
	agentId: string;
	sessionIds: Array<string | null | undefined>;
	/** Durable / loaded lines used for the fallback user-turn match. */
	referenceLines?: ChatLine[];
}): ChatLine[] | null {
	const exact = loadCachedTranscript(input.agentId, input.sessionIds);
	if (exact && hasVisibleAssistantText(exact)) return exact;
	const reference = input.referenceLines;
	if (!reference || reference.length === 0) return exact;
	const matched = findCachedTranscriptByUserTurns(input.agentId, reference);
	if (matched && hasVisibleAssistantText(matched)) return matched;
	return exact;
}

/** Remove one conversation from the cache (deleted / gone sessions). */
export function removeCachedTranscript(
	agentId: string,
	sessionIds: Array<string | null | undefined> | string | null | undefined,
): void {
	const agent = cleanId(agentId);
	if (!agent) return;
	const ids = Array.isArray(sessionIds) ? sessionIds : [sessionIds];
	const entries = readRawEntries();
	let changed = false;
	for (const sessionId of ids) {
		const session = cleanId(typeof sessionId === "string" ? sessionId : "");
		if (!session) continue;
		const key = transcriptCacheKey(agent, session);
		if (!(key in entries)) continue;
		delete entries[key];
		changed = true;
	}
	if (changed) writeCacheFile(entries);
}

/** Forget every cached transcript (testing / explicit reset only). */
export function clearCachedTranscripts(): void {
	removeStorageKey(TRANSCRIPT_CACHE_STORAGE_KEY);
}

// ---------------------------------------------------------------------------
// Debounced save scheduler (used by the persistence hook)
// ---------------------------------------------------------------------------

export type ScheduleTranscriptSaveOptions = {
	/**
	 * Cache keys to delete after the new key is written. Used when a runtime
	 * session id is upgraded to the durable provider session id: the stale
	 * runtime-id entry would otherwise duplicate the transcript.
	 */
	dropSessionIds?: Array<string | null | undefined>;
};

type PendingSave = {
	agentId: string;
	sessionId: string;
	lines: ChatLine[];
	dropSessionIds: string[];
};

const pendingSaves = new Map<string, PendingSave>();
let flushTimer: ReturnType<typeof setTimeout> | null = null;
let flushDeadline = 0;

function armFlushTimer(): void {
	const now = Date.now();
	if (flushDeadline === 0) flushDeadline = now + MAX_SAVE_DELAY_MS;
	if (flushTimer !== null) clearTimeout(flushTimer);
	const wait = Math.max(
		0,
		Math.min(TRANSCRIPT_SAVE_DEBOUNCE_MS, flushDeadline - now),
	);
	flushTimer = setTimeout(flushScheduledTranscriptSaves, wait);
}

/**
 * Queue a transcript save. Coalesces streaming updates into one write per
 * quiet window, but never starves a long stream (hard 2 s upper bound).
 */
export function scheduleTranscriptSave(
	agentId: string,
	sessionId: string,
	lines: ChatLine[],
	options?: ScheduleTranscriptSaveOptions,
): void {
	const agent = cleanId(agentId);
	const session = cleanId(sessionId);
	if (!agent || !session) return;
	const key = transcriptCacheKey(agent, session);
	const previous = pendingSaves.get(key);
	const dropSessionIds = new Set(previous?.dropSessionIds ?? []);
	for (const drop of options?.dropSessionIds ?? []) {
		const cleaned = cleanId(typeof drop === "string" ? drop : "");
		if (cleaned && cleaned !== session) dropSessionIds.add(cleaned);
	}
	pendingSaves.set(key, {
		agentId: agent,
		sessionId: session,
		lines,
		dropSessionIds: [...dropSessionIds],
	});
	armFlushTimer();
}

/** Write every pending transcript immediately (page hide / unmount / tests). */
export function flushScheduledTranscriptSaves(): void {
	if (flushTimer !== null) clearTimeout(flushTimer);
	flushTimer = null;
	flushDeadline = 0;
	if (pendingSaves.size === 0) return;
	const pending = [...pendingSaves.values()];
	pendingSaves.clear();
	for (const item of pending) {
		const saved = saveCachedTranscript(
			item.agentId,
			item.sessionId,
			item.lines,
		);
		// Keep the old runtime-id copy when the migrated write failed (quota /
		// storage unavailable): it may be the only copy of the transcript.
		if (!saved) continue;
		for (const drop of item.dropSessionIds) {
			removeCachedTranscript(item.agentId, [drop]);
		}
	}
}

/** Drop the pending queue without writing (tests / teardown safety). */
export function discardScheduledTranscriptSaves(): void {
	if (flushTimer !== null) clearTimeout(flushTimer);
	flushTimer = null;
	flushDeadline = 0;
	pendingSaves.clear();
}

// ---------------------------------------------------------------------------
// Store mirror (React hook delegates here so it is unit-testable)
// ---------------------------------------------------------------------------

/** Minimal session shape the persister needs from the Agent session store. */
export type PersistentChatSession = {
	id: string;
	agentId: string;
	providerSessionId?: string | null;
	lines: ChatLine[];
};

export type TranscriptPersister = {
	/** Queue debounced saves for sessions whose transcript changed. */
	sync: (sessions: PersistentChatSession[]) => void;
};

/**
 * Mirror store sessions into the cache, skipping unchanged transcripts.
 *
 * The cache key is the durable provider session id when known and the runtime
 * session id before `agent:completed`; upgrading the key drops the old entry.
 * Session rows disappearing from the store (panel/agent/vault scope resets)
 * must NOT delete cache entries — surviving those resets is the whole point.
 */
export function createTranscriptPersister(): TranscriptPersister {
	const scheduledLines = new Map<string, ChatLine[]>();
	const scheduledKeys = new Map<string, string>();
	let lastSessions: PersistentChatSession[] | null = null;
	return {
		sync: (sessions) => {
			if (sessions === lastSessions) return;
			lastSessions = sessions;
			const liveSessionIds = new Set<string>();
			for (const session of sessions) {
				liveSessionIds.add(session.id);
				const lines = session.lines;
				if (lines.length === 0) {
					scheduledLines.delete(session.id);
					continue;
				}
				// Cache under the durable conversation identity once it is known;
				// before `agent:completed` that is the runtime session id.
				const providerId = session.providerSessionId?.trim();
				const nextKey = providerId || session.id;
				const previousKey = scheduledKeys.get(session.id);
				if (
					scheduledLines.get(session.id) === lines &&
					previousKey === nextKey
				) {
					continue;
				}
				scheduledLines.set(session.id, lines);
				scheduledKeys.set(session.id, nextKey);
				scheduleTranscriptSave(
					session.agentId,
					nextKey,
					lines,
					previousKey && previousKey !== nextKey
						? { dropSessionIds: [previousKey] }
						: undefined,
				);
			}
			for (const sessionId of [...scheduledLines.keys()]) {
				if (liveSessionIds.has(sessionId)) continue;
				scheduledLines.delete(sessionId);
				scheduledKeys.delete(sessionId);
			}
		},
	};
}

// ---------------------------------------------------------------------------
// Merge (durable transcript wins; cache only fills gaps)
// ---------------------------------------------------------------------------

function agentLineHasText(line: ChatLine): boolean {
	return (
		line.kind === "agent" && agentTextFromParts(line.parts).trim().length > 0
	);
}

function agentHasAnyTextPart(line: AgentChatLine): boolean {
	return line.parts.some(
		(part) => part.type === "text" && part.text.trim().length > 0,
	);
}

/**
 * Merge a cached conversation into the durable/ACP transcript.
 *
 * Durable structure (user turns, reasoning, tool calls, notices) is kept as-is.
 * For each durable agent line without visible text, the matching cached agent
 * line (same agent ordinal — host replay emits one agent line per turn)
 * contributes its text parts. Turns the durable transcript lost completely are
 * appended, deduplicating user turns that already exist.
 */
export function mergeTranscriptLines(
	durable: ChatLine[],
	cached: ChatLine[],
): ChatLine[] {
	const restored = normalizeRestoredLines(cached);
	if (restored.length === 0) return durable;
	if (durable.length === 0) return restored;
	if (!hasVisibleAssistantText(restored)) return durable;

	const cachedAgentIndexes: number[] = [];
	const cachedAgentLines: AgentChatLine[] = [];
	for (let index = 0; index < restored.length; index++) {
		const line = restored[index];
		if (line.kind !== "agent") continue;
		cachedAgentIndexes.push(index);
		cachedAgentLines.push(line);
	}

	const merged = durable.slice();
	let changed = false;
	let durableAgentCount = 0;
	for (let index = 0; index < merged.length; index++) {
		const line = merged[index];
		if (line.kind !== "agent") continue;
		const cachedAgent = cachedAgentLines[durableAgentCount];
		durableAgentCount++;
		if (!cachedAgent || !agentHasAnyTextPart(cachedAgent)) continue;
		if (agentLineHasText(line)) continue;
		merged[index] = mergeAgentLine(line, cachedAgent);
		changed = true;
	}

	if (cachedAgentLines.length > durableAgentCount) {
		const firstExtraAgent = cachedAgentIndexes[durableAgentCount];
		if (firstExtraAgent !== undefined) {
			changed = appendCachedTurns(merged, restored, firstExtraAgent) || changed;
		}
	}
	return changed ? merged : durable;
}

function mergeAgentLine(
	durable: AgentChatLine,
	cached: AgentChatLine,
): AgentChatLine {
	const cachedTextParts = cached.parts.filter(
		(part): part is Extract<AgentPart, { type: "text" }> =>
			part.type === "text" && part.text.trim().length > 0,
	);
	// Cache is authoritative only for missing text. Durable reasoning/tool order
	// stays intact; a tool left pending by the interrupted turn is resolved from
	// the cached snapshot when possible.
	const cachedTools = new Map<string, ToolUiState>();
	for (const part of cached.parts) {
		if (part.type === "tool") cachedTools.set(part.tool.id, part.tool);
	}
	const cachedHasText = cachedTextParts.length > 0;
	let parts: AgentPart[];
	if (durable.parts.length === 0) {
		parts = cached.parts;
	} else {
		const base = durable.parts.map((part) => {
			if (part.type !== "tool") return part;
			const cachedTool = cachedTools.get(part.tool.id);
			if (!cachedTool) return part;
			const incomplete =
				part.tool.status === "pending" || part.tool.status === "in_progress";
			const resolved =
				cachedTool.status === "completed" || cachedTool.status === "failed";
			if (!incomplete && !resolved) return part;
			return {
				...part,
				tool: {
					...part.tool,
					status: resolved ? cachedTool.status : part.tool.status,
					input: part.tool.input ?? cachedTool.input,
					output: part.tool.output ?? cachedTool.output,
				},
			};
		});
		// Whitespace-only durable text parts are dead weight once cache text is used.
		const withoutBlankText = base.filter(
			(part) => !(part.type === "text" && part.text.trim().length === 0),
		);
		parts = cachedHasText ? [...withoutBlankText, ...cachedTextParts] : base;
	}
	const sources = durable.sources?.length ? durable.sources : cached.sources;
	return {
		id: durable.id,
		kind: "agent",
		parts: failIncompleteTools(parts),
		streaming: false,
		...(sources?.length ? { sources: [...sources] } : {}),
	};
}

function appendCachedTurns(
	merged: ChatLine[],
	cached: ChatLine[],
	firstExtraAgentIndex: number,
): boolean {
	// Include the user line that opened the turn holding the first extra agent line.
	let start = firstExtraAgentIndex;
	while (start > 0) {
		const previous = cached[start - 1];
		if (previous.kind === "user") {
			start--;
			break;
		}
		start--;
	}
	const seenUserTexts = new Set<string>();
	for (const line of merged) {
		const text = userTurnText(line);
		if (text) seenUserTexts.add(text);
	}
	let currentGroup: ChatLine[] | null = null;
	const groups: ChatLine[][] = [];
	for (let index = start; index < cached.length; index++) {
		const line = cached[index];
		if (line.kind === "user") {
			currentGroup = [line];
			groups.push(currentGroup);
			continue;
		}
		if (!currentGroup) {
			currentGroup = [];
			groups.push(currentGroup);
		}
		currentGroup.push(line);
	}
	let appended = false;
	for (const group of groups) {
		const answerLines = group.filter(
			(line): line is AgentChatLine =>
				line.kind === "agent" && agentHasAnyTextPart(line),
		);
		if (answerLines.length === 0) continue;
		const userLine = group.find(isUserChatLine);
		if (userLine) {
			const text = userTurnText(userLine);
			if (!text || !seenUserTexts.has(text)) {
				if (text) seenUserTexts.add(text);
				merged.push(userLine);
				appended = true;
			}
		}
		for (const answerLine of answerLines) {
			merged.push(answerLine);
			appended = true;
		}
	}
	return appended;
}

/** Visible transcript text summary for tests / diagnostics. */
export function transcriptText(lines: ChatLine[]): string {
	return lines
		.map((line) => {
			if (line.kind === "agent") return agentTextFromParts(line.parts);
			if ("text" in line) return line.text;
			return "";
		})
		.filter(Boolean)
		.join("\n");
}

export type { AgentChatLine, UserChatLine };
