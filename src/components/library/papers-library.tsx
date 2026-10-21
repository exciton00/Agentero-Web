/**
 * Vault library: table of all papers from catalog.sqlite (display only).
 * Click column headers to sort ascending / descending.
 * Title-header inline search + tags-header filter (no separate toolbar).
 * Single-click a row to open the paper (deferred so double-click can cancel);
 * double-click a cell to copy that field.
 * Reading heat: title text background as a left→right spine (doc start→end).
 */
import { useVirtualizer } from "@tanstack/react-virtual";
import { RefreshCw, X } from "lucide-react";
import type { ReactNode } from "react";
import {
	type KeyboardEvent as ReactKeyboardEvent,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { useTranslation } from "react-i18next";
import { AddPaperTagsDialog } from "@/components/library/add-paper-tags-dialog";
import { useCellCopy } from "@/components/library/hooks/use-cell-copy";
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
import { PaperTagChip } from "@/components/library/paper-tag-chip";
import { Button } from "@/components/ui/button";
import { useUiStore } from "@/hooks/use-app-stores";
import { useDebouncedCallback } from "@/hooks/use-debounce";
import { notifyError, notifySuccess } from "@/lib/core/notify";
import { cn } from "@/lib/core/utils";
import type { PaperLibraryRow, PaperMetadata } from "@/lib/paper";
import { filterPapersByScope, rescanPapers } from "@/lib/paper/api";
import {
	refreshPaperMetadata,
	syncLibraryPublication,
} from "@/lib/paper/library-actions";
import { refreshLibrary } from "@/lib/paper/library-store";
import { canonicalPaperPdfPath } from "@/lib/paper/media";
import { heatmapCacheKey } from "@/lib/paper/reading-heatmap";
import { editablePaperTags, type PaperTag } from "@/lib/paper/tags";
import {
	deriveTagsIndexFromPapers,
	refreshTagsIndex,
	useTagsIndex,
} from "@/lib/paper/tags-index";
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
	/** Row whose "add tags" dialog is open (context menu action). */
	const [tagEditorPaper, setTagEditorPaper] = useState<PaperMetadata | null>(
		null,
	);
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

	const selectPaper = useCallback((paper: PaperMetadata) => {
		const rel = (paper.path ?? "")
			.replace(/\\/g, "/")
			.replace(/^\/+|\/+$/g, "");
		setSelectedPaperPath(rel || paper.id);
		setHoveredPaperPath(rel || null);
	}, []);

	const { onCellCopy, onRowClick, openPaperFromRow } = useCellCopy({
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
	const columnWidths = useMemo(
		() =>
			resizing ??
			resolveColumnWidths(
				visibleColumns,
				viewportRem,
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
				notifySuccess(t("papersLibrary.uploadPdfDone"));
			} catch (error) {
				notifyError(t("papersLibrary.uploadPdfFailed"), {
					description: error instanceof Error ? error.message : String(error),
				});
			}
		},
		[t, vaultPath],
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
				if (!byName.has(tag.name)) byName.set(tag.name, tag);
			}
		}
		// Keep catalog colors authoritative, but surface index-only tags too
		// (a tag that exists vault-wide but not in the current folder scope).
		for (const tag of tagIndex.tags) {
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
			filtered = filtered.filter((row) =>
				row.searchText.includes(normalizedQuery),
			);
		}
		if (tagFilterSet.size > 0) {
			// OR across the selected tags.
			filtered = filtered.filter((row) =>
				row.tags.some((tag) => tagFilterSet.has(tag.name)),
			);
		} else {
			// No tag selected: show untagged papers only (explicit product
			// requirement — tagged papers surface through the tag filter).
			filtered = filtered.filter((row) => row.tags.length === 0);
		}
		const copy = [...filtered];
		copy.sort((a, b) => comparePaperRows(a, b, sortKey, sortDir));
		return copy;
	}, [indexedRows, normalizedQuery, tagFilterSet, sortKey, sortDir]);

	// Reorder cue: replay a quick fade when the user re-sorts or changes tag
	// filters. Deliberately excludes the search query — per-keystroke remounts
	// would flicker (high-frequency input, motion-15).
	const reorderKey = useMemo(
		() => `${sortKey}:${sortDir}:${tagFilter.join(",")}`,
		[sortKey, sortDir, tagFilter],
	);

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

	/** Enter opens the selected row; ArrowUp/Down move the selection. */
	const onTableKeyDown = useCallback(
		(event: ReactKeyboardEvent<HTMLDivElement>) => {
			if (event.key === "Enter") {
				const selected = rows.find(
					(row) =>
						(selectedPaperPath ?? "") ===
						(row.paper.path ?? "")
							.replace(/\\/g, "/")
							.replace(/^\/+|\/+$/g, ""),
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
				(row) =>
					(selectedPaperPath ?? "") ===
					(row.paper.path ?? "").replace(/\\/g, "/").replace(/^\/+|\/+$/g, ""),
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
		[rows, selectedPaperPath, openPaperFromRow, selectPaper],
	);

	/** Clear a selection that the current tag/scope filter hides. */
	useEffect(() => {
		if (!selectedPaperPath) return;
		const visible = rows.some(
			(row) =>
				(row.paper.path ?? "").replace(/\\/g, "/").replace(/^\/+|\/+$/g, "") ===
				selectedPaperPath,
		);
		if (!visible) setSelectedPaperPath(null);
	}, [rows, selectedPaperPath]);

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
					<div className="sticky left-0 z-10 flex w-fit min-w-full flex-wrap items-center gap-1.5 border-border/40 border-b bg-background/85 px-3 py-1.5 backdrop-blur">
						{tagFilterActive ? (
							<>
								<span className="text-muted-foreground text-xs">
									{t("papersLibrary.tagFilterLabel")}
								</span>
								{tagFilter.map((name) => {
									const tag = availableTags.find(
										(candidate) => candidate.name === name,
									) ?? { name };
									return (
										<PaperTagChip
											key={name}
											tag={tag}
											title={t("papersLibrary.clearTagFilter")}
											trailing={<X className="size-2.5" aria-hidden />}
											onClick={() => toggleTagFilter(name)}
										/>
									);
								})}
								<Button
									type="button"
									variant="ghost"
									size="xs"
									className="h-6 px-1.5 text-xs"
									onClick={clearTagFilter}
								>
									{t("papersLibrary.clearTagFilter")}
								</Button>
							</>
						) : (
							<span className="text-muted-foreground text-xs">
								{t("papersLibrary.untaggedOnlyHint")}
							</span>
						)}
					</div>
					{/* Fixed weights keep the table stable while content and rows change. */}
					<table
						className="table-fixed border-collapse text-left text-sm"
						style={{
							width: `${columnWidths.reduce((sum, width) => sum + width, 0)}rem`,
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
						/>
						{/* key={reorderKey} remounts the tbody to replay the fade animation. */}
						<tbody
							key={reorderKey}
							className="animate-in fade-in-0 duration-150"
						>
							{paddingTop > 0 ? (
								<tr aria-hidden>
									<td
										colSpan={visibleColumns.length}
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
											(selectedPaperPath ?? "") ===
											(p.path ?? "")
												.replace(/\\/g, "/")
												.replace(/^\/+|\/+$/g, "")
										}
										onOpenPaper={openPaperFromRow}
										onAddTags={setTagEditorPaper}
										onRequestPdfUpload={requestPdfUpload}
										onRowClick={onRowClick}
										onRefreshMetadata={handleRefreshPaperMetadata}
										onHoverPaper={setHoveredPaperPath}
										measureRef={rowVirtualizer.measureElement}
									/>
								);
							})}
							{paddingBottom > 0 ? (
								<tr aria-hidden>
									<td
										colSpan={visibleColumns.length}
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
								? t("papersLibrary.noMatch")
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
			<AddPaperTagsDialog
				open={tagEditorPaper !== null}
				paper={tagEditorPaper}
				suggestions={availableTags}
				onOpenChange={(open) => {
					if (!open) setTagEditorPaper(null);
				}}
			/>
		</LibraryPdfDropSurface>
	);
}
