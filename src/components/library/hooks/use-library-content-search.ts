/**
 * Debounced Host search for the Library content domains.
 *
 * Metadata filtering stays local and instant. Only when the user enables a
 * content domain is the (debounced) query sent to `paper_search_content`, which
 * returns the matching paper folders from its mtime/size-invalidated cache.
 * Previous matches stay visible while a new query is in flight so the table
 * does not flash empty between keystrokes.
 */

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useDebouncedValue } from "@/hooks/use-debounce";
import { notifyError } from "@/lib/core/notify";
import { searchPaperContent } from "@/lib/paper/search-content";
import type { PaperSearchDomain } from "@/lib/paper/search-domains";

/**
 * Quiet time before a keystroke reaches the content search RPC. Slightly longer
 * than the metadata commit debounce: the table already filters locally, and
 * each round-trip walks the catalog's cached text.
 */
const CONTENT_SEARCH_DEBOUNCE_MS = 300;

export type LibraryContentSearch = {
	/** Matching paper folders, or `null` while no content domain is active. */
	matchingPaths: Set<string> | null;
	/** A content query is in flight (renders a compact spinner). */
	loading: boolean;
};

export function useLibraryContentSearch(opts: {
	vaultPath: string | null | undefined;
	/** Live input value (not the debounced committed library query). */
	query: string;
	domains: readonly PaperSearchDomain[];
	/** False in the desktop shell / while the Library tab is not focused. */
	enabled: boolean;
}): LibraryContentSearch {
	const { t } = useTranslation("sidebar");
	const vaultPath = opts.vaultPath;
	const query = opts.enabled ? opts.query.trim() : "";
	const domainsKey = opts.domains.join(",");
	const debouncedQuery = useDebouncedValue(query, CONTENT_SEARCH_DEBOUNCE_MS);
	const active =
		opts.enabled &&
		Boolean(vaultPath) &&
		domainsKey.length > 0 &&
		query.length > 0;
	/** `requestLoading` tracks the RPC; debounce lag is added on the way out. */
	const [state, setState] = useState<{
		matchingPaths: Set<string> | null;
		requestLoading: boolean;
	}>({ matchingPaths: null, requestLoading: false });
	/** Drop out-of-order responses from superseded queries. */
	const requestIdRef = useRef(0);
	/** Only toast a given failure once (each keystroke would re-fail). */
	const lastErrorRef = useRef<string | null>(null);

	useEffect(() => {
		if (!active) {
			requestIdRef.current += 1;
			lastErrorRef.current = null;
			setState({ matchingPaths: null, requestLoading: false });
			return;
		}
		// Typed but still inside the debounce window: show activity, keep rows.
		if (!debouncedQuery) {
			setState((prev) =>
				prev.requestLoading ? prev : { ...prev, requestLoading: true },
			);
			return;
		}
		const domains = domainsKey.split(",") as PaperSearchDomain[];
		const requestId = ++requestIdRef.current;
		let cancelled = false;
		setState((prev) => ({
			matchingPaths: prev.matchingPaths,
			requestLoading: true,
		}));
		searchPaperContent({ vaultPath, query: debouncedQuery, domains })
			.then((matchingPaths) => {
				if (cancelled || requestId !== requestIdRef.current) return;
				lastErrorRef.current = null;
				setState({ matchingPaths, requestLoading: false });
			})
			.catch((error: unknown) => {
				if (cancelled || requestId !== requestIdRef.current) return;
				// Degrade to metadata-only matches rather than blank the table.
				setState({ matchingPaths: new Set(), requestLoading: false });
				const description =
					error instanceof Error ? error.message : String(error);
				if (lastErrorRef.current !== description) {
					lastErrorRef.current = description;
					notifyError(t("papersLibrary.searchDomains.searchFailed"), {
						description,
					});
				}
			});
		return () => {
			cancelled = true;
		};
	}, [active, debouncedQuery, domainsKey, vaultPath, t]);

	return {
		matchingPaths: state.matchingPaths,
		// Show activity during the debounce window too, not only while the RPC
		// is on the wire; once the debounced query catches up, the request state
		// takes over.
		loading: active && (state.requestLoading || query !== debouncedQuery),
	};
}
