/**
 * Library content-domain search over the Host (`paper_search_content`).
 *
 * The Library filters catalog metadata locally and instantly; when the user
 * enables extra search domains (NOTES / highlights / captions) the query is
 * also sent to the Host, which matches cached per-paper content and answers
 * with the matching vault-relative paper folders only. Callers merge those
 * paths into the local row filter (union with metadata matches).
 */

import { invoke } from "@tauri-apps/api/core";
import { hasHostTransport } from "@/lib/core/tauri";
import type { PaperSearchDomain } from "@/lib/paper/search-domains";

/** `ApiResult<T>` envelope returned by Host commands. */
type Envelope<T> = {
	ok?: boolean;
	data?: T | null;
	error?: { message?: string } | null;
};

/** Wire payload of `paper_search_content`. */
type PaperContentSearchWire = {
	paperPaths?: unknown;
};

/** Normalize a vault-relative path for set membership. */
export function normalizePaperPath(path: string | null | undefined): string {
	return (path ?? "").replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
}

/**
 * Whether one catalog row matches the current query.
 *
 * Metadata is always matched locally (the default, unchanged behavior). When
 * content matches are provided, a row also matches if its folder is in that
 * set. `contentMatches` is `null` while no content domain is active.
 */
export function matchesLibrarySearch(
	metadataHaystack: string,
	paperPath: string | null | undefined,
	normalizedQuery: string,
	contentMatches: ReadonlySet<string> | null,
): boolean {
	if (!normalizedQuery) return true;
	if (metadataHaystack.includes(normalizedQuery)) return true;
	if (!contentMatches?.size) return false;
	const rel = normalizePaperPath(paperPath);
	return rel.length > 0 && contentMatches.has(rel);
}

function hostErrorMessage(error: unknown): string {
	if (error instanceof Error) return error.message;
	if (typeof error === "string") return error;
	const message = (error as { message?: unknown } | null)?.message;
	return typeof message === "string" ? message : "paper_search_content failed";
}

/**
 * Ask the Host which catalog papers match `query` inside `domains`.
 *
 * Only the matching paper paths come back (never the source text). Returns an
 * empty set when the call is inactive/unsupported so metadata-only search keeps
 * working against older hosts. Throws on a Host command failure so the caller
 * can surface it once.
 */
export async function searchPaperContent(opts: {
	vaultPath: string | null | undefined;
	query: string;
	domains: readonly PaperSearchDomain[];
}): Promise<Set<string>> {
	const query = opts.query.trim();
	const domains = [...opts.domains];
	if (
		!opts.vaultPath ||
		!hasHostTransport() ||
		!query ||
		domains.length === 0
	) {
		return new Set();
	}

	let response: Envelope<PaperContentSearchWire>;
	try {
		response = await invoke<Envelope<PaperContentSearchWire>>(
			"paper_search_content",
			{ args: { vaultPath: opts.vaultPath, query, domains } },
		);
	} catch (error) {
		throw new Error(hostErrorMessage(error));
	}
	// Older hosts / desktop builds answer with the unknown-command stub: treat
	// that as "no content matches" instead of failing the whole search.
	if (response?.ok !== true) {
		const message = response?.error?.message?.trim();
		if (message) throw new Error(message);
		return new Set();
	}

	const paths = response.data?.paperPaths;
	if (!Array.isArray(paths)) return new Set();
	const matches = new Set<string>();
	for (const path of paths) {
		if (typeof path !== "string") continue;
		const rel = normalizePaperPath(path);
		if (rel) matches.add(rel);
	}
	return matches;
}
