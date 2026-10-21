/**
 * Vault-wide tag index for the Library tag filter.
 *
 * The server (and, once ported, the desktop Host) exposes
 * `tags_index_read({ vaultPath })` → `{ generatedAt, tags, papers, untagged }`
 * backed by the hidden `.agentero/tag-index.json` cache. Internal tags
 * (`@zotero:` / `@arxiv:`) are already filtered out server-side.
 *
 * Until the RPC exists (older hosts / desktop builds without the command), the
 * module resolves to `null` and callers fall back to deriving tags from the
 * already-loaded library rows — the filter keeps working with catalog data.
 */

import { invoke } from "@tauri-apps/api/core";
import { useEffect, useSyncExternalStore } from "react";
import { hasHostTransport } from "@/lib/core/tauri";
import type { PaperLibraryRow } from "@/lib/paper";
import { editablePaperTags, type PaperTag } from "@/lib/paper/tags";

export type TagsIndexTag = {
	name: string;
	/** Tag color token (see `lib/ui/tag-colors`), absent for default chips. */
	color?: string | null;
	/** Number of papers carrying this tag. */
	count: number;
};

export type TagsIndexPaper = {
	/** Vault-relative paper folder path. */
	path: string;
	tags: PaperTag[];
};

export type TagsIndex = {
	generatedAt: string;
	tags: TagsIndexTag[];
	papers: TagsIndexPaper[];
	/** Vault-relative paths of papers without any user tag. */
	untagged: string[];
};

function normalizePath(path: string): string {
	return path.replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
}

function asArray(value: unknown): unknown[] {
	return Array.isArray(value) ? value : [];
}

function normalizeTag(raw: unknown): TagsIndexTag | null {
	if (typeof raw !== "object" || raw === null) return null;
	const entry = raw as { name?: unknown; color?: unknown; count?: unknown };
	if (typeof entry.name !== "string" || !entry.name.trim()) return null;
	return {
		name: entry.name,
		color: typeof entry.color === "string" ? entry.color : null,
		count: typeof entry.count === "number" ? entry.count : 0,
	};
}

function normalize(raw: unknown): TagsIndex | null {
	if (typeof raw !== "object" || raw === null) return null;
	const data = raw as {
		generatedAt?: unknown;
		tags?: unknown;
		papers?: unknown;
		untagged?: unknown;
	};
	const tags = asArray(data.tags)
		.map(normalizeTag)
		.filter((tag): tag is TagsIndexTag => tag !== null)
		.sort((a, b) =>
			a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
		);
	const papers: TagsIndexPaper[] = [];
	for (const entry of asArray(data.papers)) {
		if (typeof entry !== "object" || entry === null) continue;
		const paper = entry as { path?: unknown; tags?: unknown };
		if (typeof paper.path !== "string" || !paper.path.trim()) continue;
		papers.push({
			path: normalizePath(paper.path),
			tags: editablePaperTags(asArray(paper.tags) as PaperTag[]) as PaperTag[],
		});
	}
	const untagged = asArray(data.untagged).filter(
		(path): path is string => typeof path === "string",
	);
	return {
		generatedAt: typeof data.generatedAt === "string" ? data.generatedAt : "",
		tags,
		papers,
		untagged: untagged.map(normalizePath),
	};
}

type Cache = { vaultPath: string; index: TagsIndex };
let cache: Cache | null = null;
let inflight: { vaultPath: string; promise: Promise<TagsIndex | null> } | null =
	null;
const listeners = new Set<() => void>();

function emit(): void {
	for (const listener of listeners) listener();
}

/** Subscribe to tag-index refreshes (for `useSyncExternalStore`). */
export function subscribeTagsIndex(listener: () => void): () => void {
	listeners.add(listener);
	return () => {
		listeners.delete(listener);
	};
}

/** Cached index for `vaultPath`, or `null` when it has not loaded yet. */
export function getTagsIndex(vaultPath: string | null): TagsIndex | null {
	if (!vaultPath || cache?.vaultPath !== vaultPath) return null;
	return cache.index;
}

async function readTagsIndex(vaultPath: string): Promise<TagsIndex | null> {
	if (!hasHostTransport() || !vaultPath) return null;
	try {
		// Accept both the `{ args: … }` wrapper (collection-style commands) and
		// the flat argument object; `parse_args` on the host tolerates either.
		const response = await invoke<{ ok?: boolean; data?: unknown } | null>(
			"tags_index_read",
			{ args: { vaultPath }, vaultPath },
		);
		if (!response || response.ok !== true) return null;
		return normalize(response.data);
	} catch {
		// Unknown command on older hosts / desktop builds: callers fall back.
		return null;
	}
}

/** Force a fresh read of the tag index; resolves with `null` on failure. */
export function refreshTagsIndex(
	vaultPath: string | null,
): Promise<TagsIndex | null> {
	if (!vaultPath) return Promise.resolve(null);
	if (inflight?.vaultPath === vaultPath) return inflight.promise;
	const promise = readTagsIndex(vaultPath)
		.then((index) => {
			if (index) {
				cache = { vaultPath, index };
				emit();
			}
			return index;
		})
		.finally(() => {
			if (inflight?.promise === promise) inflight = null;
		});
	inflight = { vaultPath, promise };
	return promise;
}

/** Load the index once per vault (no-op while cached). */
export function ensureTagsIndex(vaultPath: string | null): void {
	if (!vaultPath || getTagsIndex(vaultPath)) return;
	void refreshTagsIndex(vaultPath);
}

/** Drop the cache (vault switch, tag writes) so the next ensure re-reads. */
export function invalidateTagsIndex(vaultPath?: string | null): void {
	if (vaultPath && cache && cache.vaultPath !== vaultPath) return;
	cache = null;
	emit();
}

/**
 * Offline fallback: derive tag counts / untagged papers from the already-loaded
 * catalog rows. Colors come from the library rows; internal tags are filtered.
 */
export function deriveTagsIndexFromPapers(
	papers: readonly PaperLibraryRow[],
): TagsIndex {
	const byName = new Map<string, TagsIndexTag>();
	const indexed: TagsIndexPaper[] = [];
	const untagged: string[] = [];
	for (const paper of papers) {
		const path = normalizePath(paper.path ?? "");
		const tags = editablePaperTags(paper.tags ?? []) as PaperTag[];
		for (const tag of tags) {
			const existing = byName.get(tag.name);
			if (existing) existing.count += 1;
			else
				byName.set(tag.name, {
					name: tag.name,
					color: tag.color ?? null,
					count: 1,
				});
		}
		if (path) {
			if (tags.length) indexed.push({ path, tags });
			else untagged.push(path);
		}
	}
	return {
		generatedAt: "",
		tags: [...byName.values()].sort((a, b) =>
			a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
		),
		papers: indexed,
		untagged,
	};
}

/**
 * Subscribe to the tag index for `vaultPath`.
 *
 * Returns `null` until the host answers (or when the command is unavailable);
 * callers should fall back to {@link deriveTagsIndexFromPapers} in that case.
 */
export function useTagsIndex(vaultPath: string | null): TagsIndex | null {
	const index = useSyncExternalStore(
		subscribeTagsIndex,
		() => getTagsIndex(vaultPath),
		() => null,
	);
	useEffect(() => {
		ensureTagsIndex(vaultPath);
	}, [vaultPath]);
	return index;
}
