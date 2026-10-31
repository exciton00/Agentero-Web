/**
 * Paper reference (citation) sidecar helpers.
 * Host parses references (online S2/Crossref → local bib/bbl fallback) into
 * the rebuildable `{paper}/source/agentero-cite.json`; see docs/backend/api.md
 * `paper_refs_parse` / `paper_refs_list`.
 */
import { commands } from "@/lib/core/bindings";
import { callApi, callApiResult } from "@/lib/core/ipc";

export type CitationMeta = {
	title?: string;
	authors?: string[];
	year?: number;
	venue?: string;
	doi?: string;
	arxivId?: string;
	url?: string;
};

export type CitationLocalMatch = {
	/** Vault-relative path of the matched library paper. */
	paperPath: string;
	matchBy: "doi" | "arxiv" | "title";
};

export type Citation = {
	id: string;
	rawKey?: string;
	/** In-text marker like `[12]` when bibliography order is known. */
	display?: string;
	/** Raw bibliography entry text (always present for bbl/tex sources). */
	raw?: string;
	metadata: CitationMeta;
	localMatch?: CitationLocalMatch;
	/** e.g. `bbl`, `bib`, `s2`, `bbl+s2`. */
	source: string;
	status: "resolved" | "unresolved";
};

export type CiteSidecar = {
	schemaVersion: number;
	source: { mode: string; generatedAt: string; fingerprint: string };
	citations: Citation[];
	messages: string[];
};

/** Read the existing reference sidecar; `null` when not parsed yet. */
export async function paperRefsList(
	vaultPath: string,
	path: string,
): Promise<CiteSidecar | null> {
	// Wire sidecars carry serde `null` on absent optionals; domain readers
	// treat them as absent, so fold once at the boundary.
	const sidecar = (await callApi(
		() => commands.paperRefsList({ vaultPath, path }),
		{ fallback: "paper_refs_list failed" },
	)) as CiteSidecar | null;
	return sidecar ?? null;
}

/** Parse (or force-refresh) references for one paper and persist the sidecar. */
export async function paperRefsParse(
	vaultPath: string,
	path: string,
	force = false,
): Promise<CiteSidecar> {
	return (await callApiResult(
		() => commands.paperRefsParse({ vaultPath, path, force }),
		{ fallback: "paper_refs_parse failed" },
	)) as CiteSidecar;
}

/**
 * Sidecar schema the current parser writes (`refs::SCHEMA_VERSION` in
 * `agentero-core`). Older sidecars lack the offline `PAPER.md` bibliography
 * enrichment, so opening a paper transparently re-parses them in the
 * background instead of showing `ref-N` placeholders forever.
 */
export const CITE_SCHEMA_VERSION = 2;

/** One in-flight backfill request per paper (panel + hover hook both read). */
const pendingReparseRequests = new Set<string>();

function requestReparse(vaultPath: string, path: string): void {
	const key = `${vaultPath}\u0000${path}`;
	if (pendingReparseRequests.has(key)) return;
	pendingReparseRequests.add(key);
	void callApiResult(
		() => commands.jobParseRefsEnqueue({ vaultPath, path, force: false }),
		{ fallback: "refs parse enqueue failed" },
	)
		.catch(() => undefined)
		.finally(() => {
			pendingReparseRequests.delete(key);
		});
}

/**
 * Load the reference sidecar for a paper (read-only). When it does not yet
 * exist — or predates the current parser schema — enqueue a JobCenter
 * `ParseRefs` job; the caller reloads on `job:changed`. Replaces the old
 * blocking list→parse fallback (`loadPaperRefsAuto`), whose dedup is now the
 * JobCenter's fingerprint key.
 */
export async function loadPaperRefsReadOnly(
	vaultPath: string,
	path: string,
): Promise<CiteSidecar | null> {
	const sidecar = await paperRefsList(vaultPath, path).catch(() => null);
	if (!sidecar || sidecar.schemaVersion < CITE_SCHEMA_VERSION) {
		requestReparse(vaultPath, path);
	}
	return sidecar;
}

/** A new paper that cites the library but is not imported yet. */
export type CitingCandidate = {
	s2Id: string;
	title: string;
	date: string;
	arxivId?: string;
	doi?: string;
	/** Ready for `lookupSubmit`: `arXiv:{id}` or a bare DOI. */
	identifier: string;
	/** Vault-relative paths of my papers this candidate cites. */
	citedByMine: string[];
	/** IDF-weighted overlap; the primary ranking signal. */
	weight: number;
	similarity?: number;
	citationCount: number;
	oaPdfUrl?: string;
};

export type CitingScanResult = {
	generatedAt: string;
	sinceDate: string;
	libraryTotal: number;
	seedsTotal: number;
	seedsFetched: number;
	skippedMegaCited: number;
	skippedUncited: number;
	skippedUnknown: number;
	rawCiting: number;
	afterFilters: number;
	gatePassed: number;
	similarityThreshold?: number;
	candidates: CitingCandidate[];
	cancelled: boolean;
	messages: string[];
};

/**
 * Reverse citations — who cites *my* library. The opposite direction from the
 * rest of this module, and online-only: local TeX/`.bbl` cannot know it.
 *
 * `taskId` is the JobCenter job id; Host routes `job:progress` events to it
 * and polls it for cancellation.
 */
export async function libraryCitingScan(
	vaultPath: string,
	opts: {
		taskId?: string;
		sinceDays?: number;
		budget?: number;
		force?: boolean;
	} = {},
): Promise<CitingScanResult> {
	return (await callApiResult(
		() =>
			commands.libraryCitingScan({
				vaultPath,
				taskId: opts.taskId ?? null,
				sinceDays: opts.sinceDays ?? null,
				budget: opts.budget ?? null,
				force: opts.force ?? false,
			}),
		{ fallback: "library_citing_scan failed" },
	)) as CitingScanResult;
}

/** Identifier usable by magic-wand import for an unmatched citation. */
export function citationImportIdentifier(citation: Citation): string | null {
	const { arxivId, doi } = citation.metadata;
	if (arxivId?.trim()) return `arXiv:${arxivId.trim()}`;
	if (doi?.trim()) return doi.trim();
	return null;
}

/** Best external link for a citation: url → DOI resolver → arXiv abs page. */
export function citationExternalUrl(citation: Citation): string | null {
	const { url, doi, arxivId } = citation.metadata;
	if (url?.trim()) return url.trim();
	if (doi?.trim()) return `https://doi.org/${doi.trim()}`;
	if (arxivId?.trim()) return `https://arxiv.org/abs/${arxivId.trim()}`;
	return null;
}
