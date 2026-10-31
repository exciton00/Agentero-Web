/**
 * Vault library: table of all papers from catalog.sqlite (display only).
 * Click column headers to sort ascending / descending.
 * Title-header inline search + tags-header filter (no separate toolbar).
 * Single-click selects a row, double-click opens the paper; a long press
 * (~550ms) on a copyable cell copies that field instead.
 * Reading heat: title text background as a left→right spine (doc start→end).
 */
import { useVirtualizer } from "@tanstack/react-virtual";
import { RefreshCw } from "lucide-react";
import type { ReactNode } from "react";
import {
	type KeyboardEvent as ReactKeyboardEvent,
	type MouseEvent as ReactMouseEvent,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { useTranslation } from "react-i18next";
import { AddPaperTagsDialog } from "@/components/library/add-paper-tags-dialog";
import { ConfirmBatchDeleteDialog } from "@/components/library/confirm-batch-delete-dialog";
import { CustomPropertiesDialog } from "@/components/library/custom-properties-dialog";
import { useCellCopy } from "@/components/library/hooks/use-cell-copy";
import { useLibraryContentSearch } from "@/components/library/hooks/use-library-content-search";
import { useLibraryHeatmap } from "@/components/library/hooks/use-library-heatmap";
import { resolveColumnWidths } from "@/components/library/library-column-widths";
import { COLUMN_META } from "@/components/library/library-columns";
import { LibraryPaperRow } from "@/components/library/library-paper-row";
import { LibraryPdfDropSurface } from "@/components/library/library-pdf-drop-surface";
import {
	buildPaperRow,
	type CellCtx,
	type CellT,
	comparePaperRows,
	reorderColumns,
	type SortDir,
	type SortKey,
	toggleColumnVisibility,
} from "@/components/library/library-row-utils";
import { LibraryTableHeader } from "@/components/library/library-table-header";
import { LibraryTagFilter } from "@/components/library/library-tag-filter";
import {
	deletePaperAssets,
	forceReparsePaperLayout,
	type PaperAssetKind,
	type PaperAssetsDisplay,
	usePaperAssetsStatus,
} from "@/components/library/paper-assets";
import { Button } from "@/components/ui/button";
import { useUiStore } from "@/hooks/use-app-stores";
import { useDebouncedCallback } from "@/hooks/use-debounce";
import { errorMessage, notifyError, notifySuccess } from "@/lib/core/notify";
import { isWebHost } from "@/lib/core/tauri";
import { cn } from "@/lib/core/utils";
import type { PaperLibraryRow, PaperMetadata } from "@/lib/paper";
import { filterPapersByScope, rescanPapers } from "@/lib/paper/api";
import {
	type CustomProperty,
	type CustomPropertyValues,
	type CustomPropertyValuesByPath,
	createCustomPropertyOption,
	customPropertyValuesByPath,
	listCustomProperties,
	setPaperCustomPropertyValue,
} from "@/lib/paper/custom-properties";
import {
	refreshPaperMetadata,
	syncLibraryPublication,
} from "@/lib/paper/library-actions";
import { refreshLibrary } from "@/lib/paper/library-store";
import { canonicalPaperPdfPath } from "@/lib/paper/media";
import { heatmapCacheKey } from "@/lib/paper/reading-heatmap";
import { matchesLibrarySearch } from "@/lib/paper/search-content";
import {
	type PaperSearchDomain,
	readSearchDomains,
	writeSearchDomains,
} from "@/lib/paper/search-domains";
import { editablePaperTags, type PaperTag } from "@/lib/paper/tags";
import {
	deriveTagsIndexFromPapers,
	refreshTagsIndex,
	type TagsIndexTag,
	useTagsIndex,
} from "@/lib/paper/tags-index";
import { clearLayoutVaultState } from "@/lib/pdf/layout/store";
import {
	DEFAULT_LIBRARY_COLUMNS,
	type LibraryColumnPref,
	useUiScale,
} from "@/lib/settings";
import {
	clearTagFilter,
	setHoveredPaperPath,
	setSelectedPaperPath,
	setTagFilter,
	toggleTagFilter,
} from "@/lib/shell/ui-store";
import { joinVaultPath } from "@/lib/vault";
import { trashPathsAndNotify } from "@/lib/vault/actions";
import { isRemoteVaultHandle } from "@/lib/vault/remote/remote-vault";
import { refreshTree } from "@/lib/vault/store";

export type PapersLibraryProps = {
	/** Full catalog list (or pre-scoped); further filtered by `scopePath`. */
	papers: PaperLibraryRow[];
	/** Vault root; required to load per-paper reading heatmaps. */
	vaultPath?: string | null;
	/** When the Library tab is focused — reload heatmaps (after PDF activity). */
	active?: boolean;
	loading?: boolean;
	query?: string;
	/** Controlled search string (lives in App so palette/other callers can clear it). */
	onQueryChange?: (query: string) => void;
	/**
	 * Vault-relative folder scope (e.g. `papers/nlp`).
	 * Null/empty = full library. Filters by catalog `path` prefix (recursive).
	 */
	scopePath?: string | null;
	onOpenPaper: (paper: PaperMetadata) => void;
	/**
	 * Column order + visibility (persisted in settings). When omitted, all
	 * columns show in canonical order.
	 */
	columns?: LibraryColumnPref[];
	/** Persist a new column layout (reorder / show-hide / reset). */
	onColumnsChange?: (columns: LibraryColumnPref[]) => void;
	/** Rebuild the catalog from papers/ on disk (empty-state recovery). */
	onRescan?: () => void;
	rescanning?: boolean;
	className?: string;
};

/**
 * Delay before a keystroke commits to the shared library query.
 * Keeps typing local to the input so each key does not re-filter/sort the
 * full catalog (and re-render workspace subscribers of `query`).
 */
const SEARCH_DEBOUNCE_MS = 250;
/** Hide header search/actions while scrolling; restore after this idle gap. */
const HEADER_SCROLL_IDLE_MS = 450;
/** Fixed width (rem) of one custom-property column. */
const CUSTOM_PROPERTY_COLUMN_REM = 7;
/** Stable empty map shared by rows without custom values (memo friendly). */
const EMPTY_CUSTOM_VALUES: CustomPropertyValues = {};

export function PapersLibrary({
	papers,
	vaultPath = null,
	active = true,
	loading,
	query = "",
	onQueryChange,
	scopePath = null,
	onOpenPaper,
	columns = DEFAULT_LIBRARY_COLUMNS,
	onColumnsChange,
	onRescan,
	rescanning,
	className,
}: PapersLibraryProps) {
	const { t: tRaw, i18n } = useTranslation("sidebar");
	/** Column renderers take plain string keys; cast off strict resource unions. */
	const t = tRaw as unknown as CellT;
	const [sortKey, setSortKey] = useState<SortKey>("title");
	const [sortDir, setSortDir] = useState<SortDir>("asc");
	/** Selected tag names live in the shared UI store: the sidebar tag tree
	 *  and the tags-column popover drive the same OR filter. */
	const tagFilter = useUiStore((state) => state.tagFilter);
	const selectedPaperPath = useUiStore((state) => state.selectedPaperPath);
	/** Rows whose "add tags" dialog is open (single row or batch selection). */
	const [tagEditorPapers, setTagEditorPapers] = useState<
		PaperMetadata[] | null
	>(null);
	/** Multi-row selection (vault-relative keys) + shift-click anchor. */
	const [multiSelected, setMultiSelected] = useState<Set<string>>(
		() => new Set(),
	);
	const selectionAnchorRef = useRef<string | null>(null);
	const [batchDeleteOpen, setBatchDeleteOpen] = useState(false);
	const [batchBusy, setBatchBusy] = useState(false);
	/** PDF / layout-analysis badges from the host (`paper_assets_status`). */
	const { assetsByPath, refresh: refreshAssets } =
		usePaperAssetsStatus(vaultPath);
	/** Hidden PDF picker + its target paper (context-menu upload/overwrite). */
	const pdfInputRef = useRef<HTMLInputElement | null>(null);
	const pdfTargetRef = useRef<PaperMetadata | null>(null);

	/**
	 * Local mirror of `query`: keystrokes filter the (precomputed) rows here
	 * immediately, but only commit to the shared query after a debounce so
	 * workspace subscribers are not re-rendered on every key.
	 */
	const [inputValue, setInputValue] = useState(query);
	const commitSearch = useDebouncedCallback(
		(value: string) => onQueryChange?.(value),
		SEARCH_DEBOUNCE_MS,
	);

	useEffect(() => {
		setInputValue(query);
		commitSearch.cancel();
	}, [query, commitSearch]);

	useEffect(() => () => commitSearch.cancel(), [commitSearch]);

	// Preview hovered rows in the left Paper Info panel; clear once the table
	// loses focus or unmounts so a stale preview never lingers.
	useEffect(() => {
		if (!active) setHoveredPaperPath(null);
	}, [active]);
	useEffect(() => () => setHoveredPaperPath(null), []);

	const onSearchInputChange = useCallback(
		(value: string) => {
			setInputValue(value);
			commitSearch(value);
		},
		[commitSearch],
	);

	/**
	 * Optional content search domains (headless Web host RPC). Their selection
	 * is a UI preference persisted in localStorage; the desktop shell and older
	 * hosts keep the metadata-only search.
	 */
	const contentSearchAvailable = isWebHost();
	const [searchDomains, setSearchDomains] = useState<PaperSearchDomain[]>(() =>
		contentSearchAvailable ? readSearchDomains() : [],
	);
	/** Persist toggles, but never overwrite storage with the mount-time read. */
	const searchDomainsLoadedRef = useRef(false);
	useEffect(() => {
		if (!contentSearchAvailable) return;
		if (!searchDomainsLoadedRef.current) {
			searchDomainsLoadedRef.current = true;
			return;
		}
		writeSearchDomains(searchDomains);
	}, [contentSearchAvailable, searchDomains]);
	const toggleSearchDomain = useCallback((domain: PaperSearchDomain) => {
		setSearchDomains((prev) =>
			prev.includes(domain)
				? prev.filter((item) => item !== domain)
				: [...prev, domain],
		);
	}, []);
	const contentSearch = useLibraryContentSearch({
		vaultPath,
		query: inputValue,
		domains: searchDomains,
		enabled: contentSearchAvailable && active,
	});

	/**
	 * Custom property columns (catalog schema v11, headless Web host RPC):
	 * definitions live in the Vault catalog, per-paper values are edited in the
	 * table cells. Hidden on the desktop shell and older hosts without the RPC.
	 */
	const customPropertiesAvailable =
		isWebHost() && Boolean(vaultPath) && !isRemoteVaultHandle(vaultPath ?? "");
	const [customProperties, setCustomProperties] = useState<CustomProperty[]>(
		[],
	);
	const [customValues, setCustomValues] = useState<CustomPropertyValuesByPath>(
		{},
	);
	const [customPropertiesOpen, setCustomPropertiesOpen] = useState(false);
	const loadCustomProperties = useCallback(async () => {
		if (!vaultPath || !customPropertiesAvailable) return;
		try {
			const [definitions, values] = await Promise.all([
				listCustomProperties(vaultPath),
				customPropertyValuesByPath(vaultPath),
			]);
			setCustomProperties(definitions);
			setCustomValues(values);
		} catch {
			// Older host without the custom-properties RPC: hide the feature.
			setCustomProperties([]);
			setCustomValues({});
		}
	}, [customPropertiesAvailable, vaultPath]);
	useEffect(() => {
		void loadCustomProperties();
	}, [loadCustomProperties]);
	const handleSetCustomValue = useCallback(
		async (paper: PaperMetadata, propertyId: string, value: string | null) => {
			if (!vaultPath) return;
			try {
				const updated = await setPaperCustomPropertyValue(
					vaultPath,
					paper.path ?? paper.id,
					propertyId,
					value,
				);
				setCustomValues((prev) => ({
					...prev,
					[updated.path]: updated.values,
				}));
			} catch (error) {
				notifyError(errorMessage(error));
			}
		},
		[vaultPath],
	);
	const handleCreateCustomOption = useCallback(
		async (paper: PaperMetadata, propertyId: string, name: string) => {
			if (!vaultPath) return;
			try {
				const updated = await createCustomPropertyOption(
					vaultPath,
					paper.path ?? paper.id,
					propertyId,
					name,
				);
				setCustomProperties((prev) =>
					prev.map((property) =>
						property.id === updated.property.id ? updated.property : property,
					),
				);
				setCustomValues((prev) => ({
					...prev,
					[updated.path]: updated.values,
				}));
			} catch (error) {
				notifyError(errorMessage(error));
			}
		},
		[vaultPath],
	);

	const handleSort = useCallback(
		(key: SortKey) => {
			if (key === sortKey) {
				setSortDir((d) => (d === "asc" ? "desc" : "asc"));
				return;
			}
			setSortKey(key);
			// Date / citation count default to newest/highest first; text columns ascending
			setSortDir(
				key === "date" || key === "addedAt" || key === "citations"
					? "desc"
					: "asc",
			);
		},
		[sortKey],
	);

	const relKey = useCallback(
		(paper: PaperMetadata) =>
			(paper.path ?? "").replace(/\\/g, "/").replace(/^\/+|\/+$/g, "") ||
			paper.id,
		[],
	);

	const selectPaper = useCallback(
		(paper: PaperMetadata) => {
			const rel = relKey(paper);
			setSelectedPaperPath(rel);
			setMultiSelected(new Set([rel]));
			selectionAnchorRef.current = rel;
			setHoveredPaperPath(rel || null);
		},
		[relKey],
	);

	const { onCellCopy, openPaperFromRow } = useCellCopy({
		t,
		onOpenPaper,
		onSelectPaper: selectPaper,
	});

	const canEditMeta =
		Boolean(vaultPath) && !isRemoteVaultHandle(vaultPath ?? "");

	/** Folder scope first (cheap path-prefix filter on in-memory catalog). */
	const scopedPapers = useMemo(
		() => filterPapersByScope(papers, scopePath),
		[papers, scopePath],
	);

	/** Ordered, visible columns (title stays as a safety fallback). */
	const visibleColumns = useMemo(() => {
		const vis = columns.filter((c) => c.visible);
		return vis.length ? vis : columns.filter((c) => c.key === "title");
	}, [columns]);
	const [viewportRem, setViewportRem] = useState(56.25);
	const [resizing, setResizing] = useState<number[] | null>(null);
	/** Width of the fixed trailing asset-badge cell (rem); see the colgroup. */
	const ASSET_COLUMN_REM = 3.5;
	const columnWidths = useMemo(
		() =>
			resizing ??
			resolveColumnWidths(
				visibleColumns,
				Math.max(16, viewportRem - ASSET_COLUMN_REM),
				(col) => COLUMN_META[col.key].widthWeight,
			),
		[resizing, visibleColumns, viewportRem],
	);
	// PERF: stable identity keeps the memoized header from re-rendering on
	// unrelated parent renders; widths are captured per resize render.
	const resizeColumn = useCallback(
		(key: SortKey, width: number | null, commit: boolean) => {
			if (width === null) {
				setResizing(null);
				return;
			}
			// Freeze the other visible columns so every separator follows the pointer 1:1.
			const widths = columnWidths.map((value, index) =>
				visibleColumns[index].key === key ? width : value,
			);
			setResizing(commit ? null : widths);
			if (commit) {
				onColumnsChange?.(
					columns.map((col) => {
						const index = visibleColumns.findIndex(
							(visible) => visible.key === col.key,
						);
						return index < 0 ? col : { ...col, widthRem: widths[index] };
					}),
				);
			}
		},
		[columnWidths, visibleColumns, columns, onColumnsChange],
	);

	const toggleColumn = useCallback(
		(key: SortKey) => {
			if (!onColumnsChange || key === "title") return;
			onColumnsChange(toggleColumnVisibility(columns, key));
		},
		[columns, onColumnsChange],
	);

	const resetColumns = useCallback(() => {
		onColumnsChange?.(DEFAULT_LIBRARY_COLUMNS.map((c) => ({ ...c })));
	}, [onColumnsChange]);

	const handleColumnReorder = useCallback(
		(fromKey: SortKey, toKey: SortKey) => {
			if (!onColumnsChange || !fromKey) return;
			onColumnsChange(reorderColumns(columns, fromKey, toKey));
		},
		[columns, onColumnsChange],
	);

	const canSyncPublication =
		Boolean(vaultPath) && !isRemoteVaultHandle(vaultPath);
	const handleSyncPublication = useCallback(() => {
		void syncLibraryPublication(vaultPath, scopedPapers);
	}, [vaultPath, scopedPapers]);

	const handleRefreshPaperMetadata = useCallback(
		(paper: PaperMetadata) => {
			void refreshPaperMetadata(vaultPath, paper);
		},
		[vaultPath],
	);

	/** Context menu: pick a local PDF for this row. */
	const requestPdfUpload = useCallback((paper: PaperMetadata) => {
		pdfTargetRef.current = paper;
		pdfInputRef.current?.click();
	}, []);

	/**
	 * Context menu: replace/create the canonical `{paperId}.pdf` with the picked
	 * file, then rescan the catalog and refresh library + tree. Works in both
	 * shells: the web host serves the bytes, desktop writes through plugin-fs.
	 */
	const handlePdfPicked = useCallback(
		async (file: File | null | undefined) => {
			const paper = pdfTargetRef.current;
			pdfTargetRef.current = null;
			if (!file || !paper || !vaultPath) return;
			const paperDirAbs = paper.path
				? joinVaultPath(vaultPath, paper.path)
				: null;
			const targetPath = paperDirAbs
				? canonicalPaperPdfPath(paperDirAbs)
				: null;
			if (!targetPath) {
				notifyError(t("papersLibrary.uploadPdfFailed"));
				return;
			}
			try {
				const { writeFile } = await import("@tauri-apps/plugin-fs");
				await writeFile(targetPath, new Uint8Array(await file.arrayBuffer()));
				await rescanPapers(vaultPath);
				await refreshLibrary();
				void refreshTree(vaultPath, { quiet: true });
				void refreshTagsIndex(vaultPath);
				refreshAssets();
				notifySuccess(t("papersLibrary.uploadPdfDone"));
			} catch (error) {
				notifyError(t("papersLibrary.uploadPdfFailed"), {
					description: error instanceof Error ? error.message : String(error),
				});
			}
		},
		[t, vaultPath, refreshAssets],
	);

	const normalizedQuery = (inputValue ?? "").trim().toLocaleLowerCase();
	const tagFilterSet = useMemo(() => new Set(tagFilter), [tagFilter]);

	/** Vault tag index (server RPC); falls back to the loaded catalog rows. */
	const hostTagIndex = useTagsIndex(vaultPath);
	const derivedTagIndex = useMemo(
		() => deriveTagsIndexFromPapers(scopedPapers),
		[scopedPapers],
	);
	const tagIndex = hostTagIndex ?? derivedTagIndex;

	/** Unique tags in the current folder scope (for the tags-column filter menu). */
	const availableTags = useMemo(() => {
		const byName = new Map<string, PaperTag>();
		for (const p of scopedPapers) {
			for (const tag of editablePaperTags(p.tags)) {
				// Reserved host tags (`#submitted:` / `#easyscholar:`) are not
				// user-selectable filters; the index also strips its own.
				if (tag.name.startsWith("#")) continue;
				if (!byName.has(tag.name)) byName.set(tag.name, tag);
			}
		}
		// Keep catalog colors authoritative, but surface index-only tags too
		// (a tag that exists vault-wide but not in the current folder scope).
		for (const tag of tagIndex.tags) {
			if (tag.name.startsWith("#")) continue;
			if (!byName.has(tag.name)) {
				byName.set(tag.name, {
					name: tag.name,
					color: (tag.color as PaperTag["color"]) ?? null,
				});
			}
		}
		return [...byName.values()].sort((a, b) =>
			a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
		);
	}, [scopedPapers, tagIndex.tags]);

	/**
	 * Options for the single-row tag filter menu: every vault tag (the index
	 * is vault-wide) with its paper count; catalog rows fill tags the cached
	 * index has not seen yet. `#`-reserved tags are never offered.
	 */
	const tagFilterOptions = useMemo(() => {
		const indexCounts = new Map(
			tagIndex.tags.map((tag) => [tag.name, tag.count]),
		);
		const scopedCounts = new Map<string, number>();
		for (const p of scopedPapers) {
			for (const tag of editablePaperTags(p.tags)) {
				scopedCounts.set(tag.name, (scopedCounts.get(tag.name) ?? 0) + 1);
			}
		}
		return availableTags.map<TagsIndexTag>((tag) => ({
			name: tag.name,
			color: tag.color ?? null,
			count: indexCounts.get(tag.name) ?? scopedCounts.get(tag.name) ?? 0,
		}));
	}, [availableTags, tagIndex.tags, scopedPapers]);

	/** Drop filter selections that no longer exist in scope. */
	useEffect(() => {
		if (!tagFilter.length) return;
		const names = new Set(availableTags.map((t) => t.name));
		const next = tagFilter.filter((n) => names.has(n));
		if (next.length !== tagFilter.length) setTagFilter(next);
	}, [availableTags, tagFilter]);

	/** Coerce tags + sort keys once per paper list change (not per keystroke). */
	const indexedRows = useMemo(
		() => scopedPapers.map(buildPaperRow),
		[scopedPapers],
	);

	/** Keystroke-level work is filter + sort over precomputed rows only. */
	const rows = useMemo(() => {
		let filtered = indexedRows;
		if (normalizedQuery) {
			// Metadata always matches locally; content domains (when enabled)
			// union in the Host's matching paper folders.
			filtered = filtered.filter((row) =>
				matchesLibrarySearch(
					row.searchText,
					row.paper.path,
					normalizedQuery,
					contentSearch.matchingPaths,
				),
			);
		}
		if (tagFilterSet.size > 0) {
			// AND across the selected tags: a paper must carry every one of
			// them. No tag selected = no tag filtering (the whole scope shows).
			filtered = filtered.filter((row) => {
				for (const name of tagFilterSet) {
					if (!row.tags.some((tag) => tag.name === name)) return false;
				}
				return true;
			});
		}
		const copy = [...filtered];
		copy.sort((a, b) => comparePaperRows(a, b, sortKey, sortDir));
		return copy;
	}, [
		indexedRows,
		normalizedQuery,
		tagFilterSet,
		sortKey,
		sortDir,
		contentSearch.matchingPaths,
	]);

	// Reorder cue: replay a quick fade when the user re-sorts or changes tag
	// filters. Deliberately excludes the search query — per-keystroke remounts
	// would flicker (high-frequency input, motion-15).
	const reorderKey = useMemo(
		() => `${sortKey}:${sortDir}:${tagFilter.join(",")}`,
		[sortKey, sortDir, tagFilter],
	);

	/** Keep asset badges fresh after uploads / deletes / catalog refreshes. */
	// biome-ignore lint/correctness/useExhaustiveDependencies: catalog row changes are the refresh trigger
	useEffect(() => {
		refreshAssets();
	}, [refreshAssets, papers]);

	/** Papers selected via Cmd/Ctrl/Shift clicks (order = table order). */
	const selectedPapers = useMemo(
		() =>
			rows
				.filter((row) => multiSelected.has(relKey(row.paper)))
				.map((row) => row.paper),
		[rows, multiSelected, relKey],
	);

	/** Single click selects; Cmd/Ctrl toggles; Shift selects a range; dbl opens. */
	const handleRowClick = useCallback(
		(e: ReactMouseEvent<HTMLTableRowElement>, paper: PaperMetadata) => {
			const rel = relKey(paper);
			if (e.detail > 1) {
				// Double-click opens the paper and keeps the current selection.
				openPaperFromRow(paper);
				return;
			}
			if (e.shiftKey && selectionAnchorRef.current) {
				const anchorIndex = rows.findIndex(
					(row) => relKey(row.paper) === selectionAnchorRef.current,
				);
				const targetIndex = rows.findIndex((row) => relKey(row.paper) === rel);
				if (anchorIndex >= 0 && targetIndex >= 0) {
					const start = Math.min(anchorIndex, targetIndex);
					const end = Math.max(anchorIndex, targetIndex);
					const next = new Set<string>();
					for (let index = start; index <= end; index += 1) {
						next.add(relKey(rows[index].paper));
					}
					setMultiSelected(next);
					setSelectedPaperPath(rel);
					setHoveredPaperPath(rel || null);
					return;
				}
			}
			if (e.metaKey || e.ctrlKey) {
				setMultiSelected((prev) => {
					const next = new Set(prev);
					if (next.has(rel)) next.delete(rel);
					else next.add(rel);
					const keys = [...next];
					setSelectedPaperPath(keys[keys.length - 1] ?? null);
					return next;
				});
				selectionAnchorRef.current = rel;
				setHoveredPaperPath(rel || null);
				return;
			}
			setMultiSelected(new Set([rel]));
			selectionAnchorRef.current = rel;
			setSelectedPaperPath(rel);
			setHoveredPaperPath(rel || null);
		},
		[rows, relKey, openPaperFromRow],
	);

	/** Right-click: a row outside the selection becomes the only selection. */
	const handleRowContextMenu = useCallback(
		(paper: PaperMetadata) => {
			const rel = relKey(paper);
			if (multiSelected.has(rel)) return;
			setMultiSelected(new Set([rel]));
			selectionAnchorRef.current = rel;
			setSelectedPaperPath(rel);
		},
		[multiSelected, relKey],
	);

	/** Merge RPC asset flags with the catalog `has_pdf` fallback. */
	const assetsFor = useCallback(
		(paper: PaperLibraryRow): PaperAssetsDisplay => {
			const fromRpc = assetsByPath.get(relKey(paper));
			return {
				hasPdf: fromRpc?.hasPdf ?? paper.has_pdf === true,
				hasLayout: fromRpc
					? fromRpc.hasLayout || fromRpc.hasLayoutIndex
					: false,
			};
		},
		[assetsByPath, relKey],
	);

	/** Force a fresh layout analysis for one or more papers (batch friendly). */
	const handleReparsePapers = useCallback(
		async (papersToRun: PaperMetadata[]) => {
			if (!vaultPath || papersToRun.length === 0) return;
			let queued = 0;
			let failed = 0;
			for (const paper of papersToRun) {
				if (!paper.path) {
					failed += 1;
					continue;
				}
				try {
					await forceReparsePaperLayout(joinVaultPath(vaultPath, paper.path));
					queued += 1;
				} catch (error) {
					failed += 1;
					notifyError(t("papersLibrary.reparseLayoutFailed"), {
						description: errorMessage(error),
					});
				}
			}
			if (queued > 0) {
				notifySuccess(
					t("papersLibrary.reparseLayoutQueued", { count: queued }),
				);
			}
			if (queued > 0 || failed > 0) refreshAssets();
		},
		[t, vaultPath, refreshAssets],
	);

	/** Delete PDF / layout files for one or more papers, then refresh views. */
	const handleDeleteAssets = useCallback(
		async (papersToDelete: PaperMetadata[], kind: PaperAssetKind) => {
			if (!vaultPath || papersToDelete.length === 0) return;
			let removed = 0;
			let failed = 0;
			for (const paper of papersToDelete) {
				if (!paper.path) {
					failed += 1;
					continue;
				}
				try {
					const removedPaths = await deletePaperAssets({
						vaultPath,
						paperPath: paper.path,
						kind,
					});
					removed += removedPaths.length;
				} catch (error) {
					failed += 1;
					notifyError(t("papersLibrary.deleteAssetsFailed"), {
						description: errorMessage(error),
					});
				}
			}
			if (kind === "layout") clearLayoutVaultState();
			await refreshLibrary();
			void refreshTree(vaultPath, { quiet: true });
			refreshAssets();
			if (removed > 0) {
				notifySuccess(
					kind === "pdf"
						? t("papersLibrary.deletePdfDone", { count: removed })
						: t("papersLibrary.deleteLayoutDone", { count: removed }),
				);
			}
			if (failed > 0) {
				notifyError(t("papersLibrary.deleteAssetsPartial", { failed }));
			}
		},
		[t, vaultPath, refreshAssets],
	);

	const handleReparseSingle = useCallback(
		(paper: PaperMetadata) => {
			void handleReparsePapers([paper]);
		},
		[handleReparsePapers],
	);
	const handleDeletePdfSingle = useCallback(
		(paper: PaperMetadata) => {
			void handleDeleteAssets([paper], "pdf");
		},
		[handleDeleteAssets],
	);
	const handleDeleteLayoutSingle = useCallback(
		(paper: PaperMetadata) => {
			void handleDeleteAssets([paper], "layout");
		},
		[handleDeleteAssets],
	);
	const handleAddTagsSingle = useCallback((paper: PaperMetadata) => {
		setTagEditorPapers([paper]);
	}, []);
	const handleBatchAddTags = useCallback(() => {
		setTagEditorPapers(selectedPapers);
	}, [selectedPapers]);
	const batchActions = useMemo(
		() => ({
			onAddTags: handleBatchAddTags,
			onReparseLayout: () => void handleReparsePapers(selectedPapers),
			onDeletePdf: () => void handleDeleteAssets(selectedPapers, "pdf"),
			onDeleteLayout: () => void handleDeleteAssets(selectedPapers, "layout"),
			onDelete: () => setBatchDeleteOpen(true),
		}),
		[
			handleBatchAddTags,
			handleReparsePapers,
			handleDeleteAssets,
			selectedPapers,
		],
	);

	/** Batch delete (recycle bin) — always behind the count confirmation dialog. */
	const confirmBatchDelete = useCallback(async () => {
		if (!vaultPath || selectedPapers.length === 0) return;
		const absPaths = selectedPapers
			.map((paper) =>
				paper.path ? joinVaultPath(vaultPath, paper.path) : null,
			)
			.filter((path): path is string => Boolean(path));
		if (absPaths.length === 0) return;
		setBatchBusy(true);
		try {
			await trashPathsAndNotify(absPaths);
			setMultiSelected(new Set());
			setSelectedPaperPath(null);
			selectionAnchorRef.current = null;
		} finally {
			setBatchBusy(false);
			setBatchDeleteOpen(false);
		}
	}, [vaultPath, selectedPapers]);

	const scrollRef = useRef<HTMLDivElement | null>(null);
	const detachScrollRef = useRef<(() => void) | null>(null);
	/** True only while the table is scrolling; idle restores search/actions. */
	const [headerCompact, setHeaderCompact] = useState(false);
	/** Bind scroll listener when the table scroller mounts (after loading). */
	const setScrollEl = useCallback((el: HTMLDivElement | null) => {
		detachScrollRef.current?.();
		detachScrollRef.current = null;
		scrollRef.current = el;
		if (!el) {
			setHeaderCompact(false);
			return;
		}
		let idleTimer: ReturnType<typeof setTimeout> | null = null;
		const onScroll = () => {
			// Rows can unmount under a still pointer while scrolling.
			setHoveredPaperPath(null);
			setHeaderCompact((prev) => (prev ? prev : true));
			if (idleTimer) clearTimeout(idleTimer);
			idleTimer = setTimeout(() => {
				idleTimer = null;
				setHeaderCompact(false);
			}, HEADER_SCROLL_IDLE_MS);
		};
		const updateWidth = () =>
			setViewportRem(
				el.clientWidth /
					Number.parseFloat(
						getComputedStyle(el.ownerDocument.documentElement).fontSize,
					),
			);
		const observer = new ResizeObserver(updateWidth);
		observer.observe(el);
		observer.observe(el.ownerDocument.documentElement);
		updateWidth();
		el.addEventListener("scroll", onScroll, { passive: true });
		detachScrollRef.current = () => {
			if (idleTimer) clearTimeout(idleTimer);
			observer.disconnect();
			el.removeEventListener("scroll", onScroll);
		};
	}, []);
	useEffect(
		() => () => {
			detachScrollRef.current?.();
			detachScrollRef.current = null;
		},
		[],
	);
	const uiScale = useUiScale();
	useEffect(() => {
		if (scrollRef.current)
			setViewportRem(scrollRef.current.clientWidth / (16 * uiScale));
	}, [uiScale]);
	const rowVirtualizer = useVirtualizer({
		count: rows.length,
		getScrollElement: () => scrollRef.current,
		estimateSize: () => Math.round(52 * uiScale),
		overscan: 12,
	});

	const virtualRows = rowVirtualizer.getVirtualItems();
	const totalSize = rowVirtualizer.getTotalSize();
	const paddingTop = virtualRows.length > 0 ? virtualRows[0].start : 0;
	const paddingBottom =
		virtualRows.length > 0
			? totalSize - virtualRows[virtualRows.length - 1].end
			: 0;

	/** Enter opens the selection; arrows move it; Cmd/Ctrl+A selects all; Esc clears. */
	const onTableKeyDown = useCallback(
		(event: ReactKeyboardEvent<HTMLDivElement>) => {
			const mod = event.metaKey || event.ctrlKey;
			if (mod && event.key.toLowerCase() === "a") {
				event.preventDefault();
				const all = rows.map((row) => relKey(row.paper));
				setMultiSelected(new Set(all));
				const first = all[0] ?? null;
				selectionAnchorRef.current = first;
				setSelectedPaperPath(first);
				return;
			}
			if (event.key === "Escape") {
				if (multiSelected.size === 0 && !selectedPaperPath) return;
				event.preventDefault();
				setMultiSelected(new Set());
				setSelectedPaperPath(null);
				selectionAnchorRef.current = null;
				return;
			}
			if (event.key === "Enter") {
				const selected = rows.find(
					(row) => selectedPaperPath === relKey(row.paper),
				);
				if (selected) {
					event.preventDefault();
					openPaperFromRow(selected.paper);
				}
				return;
			}
			if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
			if (rows.length === 0) return;
			event.preventDefault();
			const current = rows.findIndex(
				(row) => selectedPaperPath === relKey(row.paper),
			);
			const delta = event.key === "ArrowDown" ? 1 : -1;
			const nextIndex =
				current < 0
					? delta > 0
						? 0
						: rows.length - 1
					: Math.min(rows.length - 1, Math.max(0, current + delta));
			const next = rows[nextIndex];
			if (next) selectPaper(next.paper);
		},
		[
			rows,
			relKey,
			multiSelected,
			selectedPaperPath,
			openPaperFromRow,
			selectPaper,
		],
	);

	/** Drop selected rows that the current tag/scope filter hides. */
	useEffect(() => {
		const visible = new Set(rows.map((row) => relKey(row.paper)));
		setMultiSelected((prev) => {
			const next = new Set([...prev].filter((key) => visible.has(key)));
			return next.size === prev.size ? prev : next;
		});
		if (selectedPaperPath && !visible.has(selectedPaperPath)) {
			setSelectedPaperPath(null);
		}
		if (
			selectionAnchorRef.current &&
			!visible.has(selectionAnchorRef.current)
		) {
			selectionAnchorRef.current = null;
		}
	}, [rows, relKey, selectedPaperPath]);

	const heatmaps = useLibraryHeatmap({
		vaultPath,
		scopedPapers,
		active,
		rows,
		virtualRows,
	});

	/** Stable t + onCellCopy pair shared by every memoized row. */
	const cellCtx = useMemo<Pick<CellCtx, "t" | "onCellCopy">>(
		() => ({ t, onCellCopy }),
		[t, onCellCopy],
	);

	const filtering = normalizedQuery.length > 0 || tagFilterSet.size > 0;
	const tagFilterActive = tagFilterSet.size > 0;

	let body: ReactNode;
	if (loading) {
		body = (
			<div className="flex min-h-0 flex-1 items-center justify-center text-muted-foreground text-sm">
				{t("papersLibrary.loading")}
			</div>
		);
	} else {
		const empty = rows.length === 0;
		body = (
			<>
				{/* biome-ignore lint/a11y/noStaticElementInteractions: hover only clears the Paper Info preview */}
				<div
					ref={setScrollEl}
					onMouseLeave={() => setHoveredPaperPath(null)}
					onKeyDown={onTableKeyDown}
					// biome-ignore lint/a11y/noNoninteractiveTabindex: keyboard-navigable library list (Enter opens, arrows move)
					tabIndex={0}
					data-library-table=""
					className={cn(
						"agentero-scroll-both min-w-0 outline-none",
						empty ? "h-auto" : "min-h-0 flex-1",
					)}
				>
					<LibraryTagFilter tags={tagFilterOptions} scopePath={scopePath} />
					{/* Fixed weights keep the table stable while content and rows change. */}
					<table
						className="table-fixed border-collapse text-left text-sm"
						style={{
							width: `${
								columnWidths.reduce((sum, width) => sum + width, 0) +
								3.5 +
								customProperties.length * CUSTOM_PROPERTY_COLUMN_REM
							}rem`,
						}}
					>
						<colgroup>
							{visibleColumns.map((col, index) => (
								<col
									key={col.key}
									style={{
										width: `${columnWidths[index]}rem`,
									}}
								/>
							))}
							{customProperties.map((property) => (
								<col
									key={property.id}
									style={{ width: `${CUSTOM_PROPERTY_COLUMN_REM}rem` }}
								/>
							))}
							{/* Fixed-width trailing cell for the PDF / layout badges. */}
							<col key="__assets__" style={{ width: "3.5rem" }} />
						</colgroup>
						<LibraryTableHeader
							t={t}
							columns={columns}
							visibleColumns={visibleColumns}
							sortKey={sortKey}
							sortDir={sortDir}
							onSort={handleSort}
							compact={headerCompact}
							searchEnabled={Boolean(onQueryChange)}
							inputValue={inputValue}
							onInputChange={onSearchInputChange}
							searchDomains={searchDomains}
							onToggleSearchDomain={toggleSearchDomain}
							contentSearchAvailable={contentSearchAvailable}
							contentSearchLoading={contentSearch.loading}
							canSyncPublication={canSyncPublication}
							onSyncPublication={handleSyncPublication}
							availableTags={availableTags}
							tagFilterSet={tagFilterSet}
							tagFilterActive={tagFilterActive}
							onToggleTagFilter={toggleTagFilter}
							onClearTagFilter={clearTagFilter}
							canCustomizeColumns={Boolean(onColumnsChange)}
							onToggleColumn={toggleColumn}
							onResetColumns={resetColumns}
							onColumnReorder={handleColumnReorder}
							onColumnResize={resizeColumn}
							customProperties={customProperties}
							onManageCustomProperties={
								customPropertiesAvailable
									? () => setCustomPropertiesOpen(true)
									: undefined
							}
						/>
						{/* key={reorderKey} remounts the tbody to replay the fade animation. */}
						<tbody
							key={reorderKey}
							className="animate-in fade-in-0 duration-150"
						>
							{paddingTop > 0 ? (
								<tr aria-hidden>
									<td
										colSpan={
											visibleColumns.length + 1 + customProperties.length
										}
										style={{ height: paddingTop }}
									/>
								</tr>
							) : null}
							{virtualRows.map((vr) => {
								const row = rows[vr.index];
								const p = row.paper;
								return (
									<LibraryPaperRow
										key={p.path ?? p.id}
										index={vr.index}
										row={row}
										heat={heatmaps.get(heatmapCacheKey(p))}
										visibleColumns={visibleColumns}
										ctx={cellCtx}
										paperAbsPath={
											p.path && vaultPath
												? joinVaultPath(vaultPath, p.path)
												: null
										}
										canEditMeta={canEditMeta}
										selected={
											multiSelected.has(relKey(p)) ||
											selectedPaperPath === relKey(p)
										}
										selectionCount={multiSelected.size}
										assets={assetsFor(p)}
										onOpenPaper={openPaperFromRow}
										onAddTags={handleAddTagsSingle}
										onRequestPdfUpload={requestPdfUpload}
										onRowContextMenu={handleRowContextMenu}
										onReparseLayout={handleReparseSingle}
										onDeletePdf={handleDeletePdfSingle}
										onDeleteLayout={handleDeleteLayoutSingle}
										batchActions={batchActions}
										onRowClick={handleRowClick}
										onRefreshMetadata={handleRefreshPaperMetadata}
										onHoverPaper={setHoveredPaperPath}
										customProperties={customProperties}
										customValues={
											customValues[(p.path ?? "").replace(/\\/g, "/")] ??
											EMPTY_CUSTOM_VALUES
										}
										onSetCustomValue={
											customPropertiesAvailable
												? handleSetCustomValue
												: undefined
										}
										onCreateOption={
											customPropertiesAvailable
												? handleCreateCustomOption
												: undefined
										}
										measureRef={rowVirtualizer.measureElement}
									/>
								);
							})}
							{paddingBottom > 0 ? (
								<tr aria-hidden>
									<td
										colSpan={
											visibleColumns.length + 1 + customProperties.length
										}
										style={{ height: paddingBottom }}
									/>
								</tr>
							) : null}
						</tbody>
					</table>
					{!empty ? (
						<p className="sticky left-0 border-border/40 border-t px-3 py-2 text-muted-foreground text-xs tracking-wide">
							{t("papersLibrary.count", {
								count: rows.length,
								formatted: new Intl.NumberFormat(i18n.language).format(
									rows.length,
								),
							})}
						</p>
					) : null}
				</div>
				{empty ? (
					<div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 p-8 text-center">
						<p className="font-medium text-sm">
							{filtering
								? contentSearch.loading
									? t("papersLibrary.searchDomains.searching")
									: t("papersLibrary.noMatch")
								: t("papersLibrary.emptyTitle")}
						</p>
						{filtering ? null : (
							<p className="max-w-sm text-muted-foreground text-xs">
								{t("papersLibrary.emptyHint")}
							</p>
						)}
						{!filtering && onRescan ? (
							<Button
								type="button"
								variant="outline"
								size="sm"
								className="mt-1"
								disabled={rescanning}
								onClick={onRescan}
							>
								<RefreshCw
									className={cn("size-3.5", rescanning && "animate-spin")}
								/>
								{t("papersLibrary.rescan")}
							</Button>
						) : null}
					</div>
				) : null}
			</>
		);
	}

	return (
		<LibraryPdfDropSurface scopePath={scopePath} className={className}>
			{body}
			<input
				ref={pdfInputRef}
				type="file"
				accept="application/pdf,.pdf"
				className="hidden"
				onChange={(event) => {
					const file = event.currentTarget.files?.[0] ?? null;
					event.currentTarget.value = "";
					void handlePdfPicked(file);
				}}
			/>
			<CustomPropertiesDialog
				open={customPropertiesOpen}
				onOpenChange={setCustomPropertiesOpen}
				properties={customProperties}
				onChanged={() => void loadCustomProperties()}
			/>
			<AddPaperTagsDialog
				open={tagEditorPapers !== null}
				papers={tagEditorPapers ?? undefined}
				suggestions={availableTags}
				onOpenChange={(open) => {
					if (!open) setTagEditorPapers(null);
				}}
			/>
			<ConfirmBatchDeleteDialog
				open={batchDeleteOpen}
				count={selectedPapers.length}
				busy={batchBusy}
				onOpenChange={setBatchDeleteOpen}
				onConfirm={() => void confirmBatchDelete()}
			/>
		</LibraryPdfDropSurface>
	);
}
