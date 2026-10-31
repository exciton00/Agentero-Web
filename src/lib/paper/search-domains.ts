/**
 * Persisted Library search domains.
 *
 * The Library always matches catalog metadata locally; these optional content
 * domains extend the query to per-paper user content served by the Host
 * (`paper_search_content`). Selection is a UI preference, so it lives in
 * localStorage rather than the shared settings schema.
 */

import {
	readJsonStorage,
	type StorageLike,
	writeJsonStorage,
} from "@/lib/core/storage";

/** Content domains that can be added to the metadata search. */
export type PaperSearchDomain = "notes" | "marks" | "captions";

/** Canonical order for menus + stable persistence. */
export const PAPER_SEARCH_DOMAINS: readonly PaperSearchDomain[] = [
	"notes",
	"marks",
	"captions",
];

/** i18n key per domain (under `papersLibrary.searchDomains`). */
export const PAPER_SEARCH_DOMAIN_LABEL_KEYS: Record<
	PaperSearchDomain,
	`papersLibrary.searchDomains.${PaperSearchDomain}`
> = {
	notes: "papersLibrary.searchDomains.notes",
	marks: "papersLibrary.searchDomains.marks",
	captions: "papersLibrary.searchDomains.captions",
};

/** Versioned storage key: a future shape change can never misread old values. */
export const PAPER_SEARCH_DOMAINS_STORAGE_KEY =
	"agentero.library.search-domains.v1";

function isSearchDomain(value: unknown): value is PaperSearchDomain {
	return (
		typeof value === "string" &&
		(PAPER_SEARCH_DOMAINS as readonly string[]).includes(value)
	);
}

/** Drop unknown/duplicate values and return canonical order. */
export function normalizeSearchDomains(value: unknown): PaperSearchDomain[] {
	if (!Array.isArray(value)) return [];
	const seen = new Set<PaperSearchDomain>();
	for (const item of value) {
		if (isSearchDomain(item)) seen.add(item);
	}
	return PAPER_SEARCH_DOMAINS.filter((domain) => seen.has(domain));
}

/** Read the persisted selection (empty = metadata only). */
export function readSearchDomains(
	storage?: StorageLike | null,
): PaperSearchDomain[] {
	const raw = readJsonStorage<unknown>(
		PAPER_SEARCH_DOMAINS_STORAGE_KEY,
		[],
		storage,
	);
	return normalizeSearchDomains(raw);
}

/** Persist the selection; returns false when storage is unavailable. */
export function writeSearchDomains(
	domains: readonly PaperSearchDomain[],
	storage?: StorageLike | null,
): boolean {
	return writeJsonStorage(
		PAPER_SEARCH_DOMAINS_STORAGE_KEY,
		normalizeSearchDomains([...domains]),
		storage,
	);
}
