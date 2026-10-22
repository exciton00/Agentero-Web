/**
 * One virtualized paper row for the library table: cells rendered from
 * COLUMN_META plus a fixed trailing asset-status cell (PDF / layout) and the
 * row context menu.
 *
 * Interactions: single click selects (modifiers extend the selection —
 * Cmd/Ctrl toggles, Shift selects a range), double click opens the paper,
 * long-pressing a copyable cell copies that field (see use-cell-copy.ts).
 *
 * The context menu switches to batch actions when more than one row is
 * selected (batch add tags / re-run layout / delete PDF / delete layout /
 * delete papers).
 *
 * Memoized — the parent must keep `ctx` (t + onCellCopy), `assets` and the
 * action callbacks reference-stable or every scroll re-renders all rows.
 */
import {
	BookOpen,
	Download,
	FileJson,
	FileX,
	FolderOpen,
	MessageSquarePlus,
	Pencil,
	RefreshCw,
	ScanLine,
	Tag,
	Tags,
	Trash2,
	Upload,
} from "lucide-react";
import type { MouseEvent } from "react";
import { Fragment, memo } from "react";
import { COLUMN_META } from "@/components/library/library-columns";
import {
	type CellCtx,
	isMissingLocalPdf,
	type PaperRow,
} from "@/components/library/library-row-utils";
import type { PaperAssetsDisplay } from "@/components/library/paper-assets";
import { PaperAssetsBadges } from "@/components/library/paper-assets-icons";
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuLabel,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { presentAgentComposer } from "@/lib/agent/composer-focus";
import { broadcastAgentAttachContext } from "@/lib/agent/context-attach";
import { notifyError } from "@/lib/core/notify";
import { isWebHost } from "@/lib/core/tauri";
import type { PaperMetadata } from "@/lib/paper";
import { downloadLibraryPaper } from "@/lib/paper/library-actions";
import { setEditMetaDraft } from "@/lib/paper/library-store";
import type { ReadingHeatmap } from "@/lib/paper/reading-heatmap";
import type { LibraryColumnPref } from "@/lib/settings";
import { trashPathsAndNotify } from "@/lib/vault/actions";
import { revealInFileManager, revealInOsLabelKey } from "@/lib/vault/reveal";

/** Batch actions rendered when more than one Library row is selected. */
export type LibraryBatchActions = {
	onAddTags: () => void;
	onReparseLayout: () => void;
	onDeletePdf: () => void;
	onDeleteLayout: () => void;
	onDelete: () => void;
};

type LibraryPaperRowProps = {
	/** Virtualizer index (`data-index` for measureElement). */
	index: number;
	row: PaperRow;
	heat: ReadingHeatmap | undefined;
	visibleColumns: LibraryColumnPref[];
	/** Stable `t` + `onCellCopy` pair shared by all rows (memo friendly). */
	ctx: Pick<CellCtx, "t" | "onCellCopy">;
	paperAbsPath: string | null;
	canEditMeta: boolean;
	/** Selected row highlight (single or multi selection). */
	selected: boolean;
	/** How many rows are selected in total (>1 → batch context menu). */
	selectionCount: number;
	/** PDF / layout-analysis badges for the trailing status cell. */
	assets: PaperAssetsDisplay;
	onOpenPaper: (paper: PaperMetadata) => void;
	/** Open the "add tags" editor for this row. */
	onAddTags?: (paper: PaperMetadata) => void;
	/** Pick a local PDF and upload it over the paper's canonical PDF file. */
	onRequestPdfUpload?: (paper: PaperMetadata) => void;
	/** Keep the multi-selection consistent before the context menu opens. */
	onRowContextMenu?: (paper: PaperMetadata) => void;
	/** Force a fresh layout analysis pass for this paper. */
	onReparseLayout?: (paper: PaperMetadata) => void;
	/** Delete `<paperId>.pdf`. */
	onDeletePdf?: (paper: PaperMetadata) => void;
	/** Delete `source/layout.json` + `layout-index.json`. */
	onDeleteLayout?: (paper: PaperMetadata) => void;
	batchActions?: LibraryBatchActions;
	onRowClick: (
		e: MouseEvent<HTMLTableRowElement>,
		paper: PaperMetadata,
	) => void;
	onRefreshMetadata?: (paper: PaperMetadata) => void;
	/** Preview the row in the left Paper Info panel while hovered. */
	onHoverPaper?: (relPath: string | null) => void;
	/** rowVirtualizer.measureElement — attached to the `<tr>`. */
	measureRef: (element: Element | null) => void;
};

export const LibraryPaperRow = memo(function LibraryPaperRow({
	index,
	row,
	heat,
	visibleColumns,
	ctx,
	paperAbsPath,
	canEditMeta,
	selected,
	selectionCount,
	assets,
	onOpenPaper,
	onAddTags,
	onRequestPdfUpload,
	onRowContextMenu,
	onReparseLayout,
	onDeletePdf,
	onDeleteLayout,
	batchActions,
	onRowClick,
	onRefreshMetadata,
	onHoverPaper,
	measureRef,
}: LibraryPaperRowProps) {
	const p = row.paper;
	const cellCtx: CellCtx = {
		t: ctx.t,
		onCellCopy: ctx.onCellCopy,
		heat,
		tags: row.tags,
	};
	const batch = selectionCount > 1 && Boolean(batchActions);
	return (
		<ContextMenu>
			<ContextMenuTrigger asChild>
				<tr
					data-index={index}
					ref={measureRef}
					className={
						selected
							? "border-border/40 border-b bg-accent/80 transition-colors duration-100 hover:bg-accent active:bg-accent"
							: "border-border/40 border-b transition-colors duration-100 hover:bg-accent/70 active:bg-accent"
					}
					aria-selected={selected}
					onClick={(e) => onRowClick(e, p)}
					onContextMenu={() => onRowContextMenu?.(p)}
					onMouseEnter={() => onHoverPaper?.(p.path ?? null)}
				>
					{visibleColumns.map((col) => (
						<Fragment key={col.key}>
							{COLUMN_META[col.key].render(p, cellCtx)}
						</Fragment>
					))}
					{/* Fixed trailing asset badges: PDF (red/grey) + layout (blue/grey). */}
					<td
						data-library-assets-cell=""
						className="p-0 align-middle"
						title={[
							assets.hasPdf
								? ctx.t("papersLibrary.assetsPdfPresent")
								: ctx.t("papersLibrary.assetsPdfMissing"),
							assets.hasLayout
								? ctx.t("papersLibrary.assetsLayoutPresent")
								: ctx.t("papersLibrary.assetsLayoutMissing"),
						].join(" · ")}
					>
						<div className="flex h-full items-center justify-end pr-2">
							<PaperAssetsBadges
								hasPdf={assets.hasPdf}
								hasLayout={assets.hasLayout}
								pdfLabel={
									assets.hasPdf
										? ctx.t("papersLibrary.assetsPdfPresent")
										: ctx.t("papersLibrary.assetsPdfMissing")
								}
								layoutLabel={
									assets.hasLayout
										? ctx.t("papersLibrary.assetsLayoutPresent")
										: ctx.t("papersLibrary.assetsLayoutMissing")
								}
							/>
						</div>
					</td>
				</tr>
			</ContextMenuTrigger>
			<ContextMenuContent className="w-52">
				{batch && batchActions ? (
					<>
						<ContextMenuLabel>
							{ctx.t("papersLibrary.batchMenuLabel", {
								count: selectionCount,
							})}
						</ContextMenuLabel>
						<ContextMenuItem onSelect={batchActions.onAddTags}>
							<Tags className="size-3.5" />
							{ctx.t("papersLibrary.batchAddTag")}
						</ContextMenuItem>
						<ContextMenuItem onSelect={batchActions.onReparseLayout}>
							<ScanLine className="size-3.5" />
							{ctx.t("papersLibrary.batchReparseLayout")}
						</ContextMenuItem>
						<ContextMenuSeparator />
						<ContextMenuItem
							className="text-destructive focus:text-destructive"
							onSelect={batchActions.onDeletePdf}
						>
							<FileX className="size-3.5" />
							{ctx.t("papersLibrary.batchDeletePdf")}
						</ContextMenuItem>
						<ContextMenuItem
							className="text-destructive focus:text-destructive"
							onSelect={batchActions.onDeleteLayout}
						>
							<FileJson className="size-3.5" />
							{ctx.t("papersLibrary.batchDeleteLayout")}
						</ContextMenuItem>
						<ContextMenuSeparator />
						<ContextMenuItem
							className="text-destructive focus:text-destructive"
							onSelect={batchActions.onDelete}
						>
							<Trash2 className="size-3.5" />
							{ctx.t("papersLibrary.batchDelete", {
								count: selectionCount,
							})}
						</ContextMenuItem>
					</>
				) : (
					<>
						<ContextMenuItem onSelect={() => onOpenPaper(p)}>
							<BookOpen className="size-3.5" />
							{ctx.t("papersLibrary.rowOpen")}
						</ContextMenuItem>
						{canEditMeta && isMissingLocalPdf(p) ? (
							<ContextMenuItem onSelect={() => void downloadLibraryPaper(p)}>
								<Download className="size-3.5" />
								{ctx.t("papersLibrary.rowDownloadPdf")}
							</ContextMenuItem>
						) : null}
						{canEditMeta ? (
							<ContextMenuItem onSelect={() => setEditMetaDraft(p)}>
								<Pencil className="size-3.5" />
								{ctx.t("papersLibrary.rowEditMeta")}
							</ContextMenuItem>
						) : null}
						{canEditMeta && onAddTags ? (
							<ContextMenuItem onSelect={() => onAddTags(p)}>
								<Tag className="size-3.5" />
								{ctx.t("papersLibrary.rowAddTag")}
							</ContextMenuItem>
						) : null}
						{canEditMeta && onRequestPdfUpload && paperAbsPath ? (
							<ContextMenuItem onSelect={() => onRequestPdfUpload(p)}>
								<Upload className="size-3.5" />
								{ctx.t("papersLibrary.rowUploadPdf")}
							</ContextMenuItem>
						) : null}
						{canEditMeta &&
						onRefreshMetadata &&
						(p.doi?.trim() || p.arxiv_id?.trim() || p.title?.trim()) ? (
							<ContextMenuItem onSelect={() => void onRefreshMetadata?.(p)}>
								<RefreshCw className="size-3.5" />
								{ctx.t("papersLibrary.rowRefreshMeta")}
							</ContextMenuItem>
						) : null}
						{paperAbsPath ? (
							<>
								<ContextMenuSeparator />
								<ContextMenuItem
									onSelect={() => {
										broadcastAgentAttachContext([paperAbsPath]);
										presentAgentComposer();
									}}
								>
									<MessageSquarePlus className="size-3.5" />
									{ctx.t("fileTree.addToChat")}
								</ContextMenuItem>
								{canEditMeta && !isWebHost() ? (
									<ContextMenuItem
										onSelect={() => {
											void revealInFileManager(paperAbsPath).catch(() =>
												notifyError(ctx.t("fileTree.revealFailed")),
											);
										}}
									>
										<FolderOpen className="size-3.5" />
										{ctx.t(revealInOsLabelKey())}
									</ContextMenuItem>
								) : null}
								{canEditMeta ? (
									<>
										<ContextMenuSeparator />
										{onReparseLayout ? (
											<ContextMenuItem
												className="text-destructive focus:text-destructive"
												onSelect={() => onReparseLayout(p)}
											>
												<ScanLine className="size-3.5" />
												{ctx.t("papersLibrary.rowReparseLayout")}
											</ContextMenuItem>
										) : null}
										{onDeletePdf ? (
											<ContextMenuItem
												className="text-destructive focus:text-destructive"
												onSelect={() => onDeletePdf(p)}
											>
												<FileX className="size-3.5" />
												{ctx.t("papersLibrary.rowDeletePdf")}
											</ContextMenuItem>
										) : null}
										{onDeleteLayout ? (
											<ContextMenuItem
												className="text-destructive focus:text-destructive"
												onSelect={() => onDeleteLayout(p)}
											>
												<FileJson className="size-3.5" />
												{ctx.t("papersLibrary.rowDeleteLayout")}
											</ContextMenuItem>
										) : null}
									</>
								) : null}
								<ContextMenuItem
									className="text-destructive focus:text-destructive"
									onSelect={() => void trashPathsAndNotify([paperAbsPath])}
								>
									<Trash2 className="size-3.5" />
									{ctx.t("fileTree.delete")}
								</ContextMenuItem>
							</>
						) : null}
					</>
				)}
			</ContextMenuContent>
		</ContextMenu>
	);
});
