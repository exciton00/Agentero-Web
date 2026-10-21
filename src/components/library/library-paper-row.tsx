/**
 * One virtualized paper row for the library table: cells rendered from
 * COLUMN_META plus the row context menu (open / download / edit / reveal /
 * delete / add-to-chat).
 *
 * Single-click opens the paper; double-click a copyable cell copies that field.
 *
 * Memoized — the parent must keep `ctx` (t + onCellCopy) and `measureRef`
 * reference-stable or every scroll re-renders all visible rows.
 */
import {
	BookOpen,
	Download,
	FolderOpen,
	MessageSquarePlus,
	Pencil,
	RefreshCw,
	Tag,
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
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
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
	/** Selected row highlight (single-click selects, Paper Info follows). */
	selected: boolean;
	onOpenPaper: (paper: PaperMetadata) => void;
	/** Open the "add tags" editor for this row. */
	onAddTags?: (paper: PaperMetadata) => void;
	/** Pick a local PDF and upload it over the paper's canonical PDF file. */
	onRequestPdfUpload?: (paper: PaperMetadata) => void;
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
	onOpenPaper,
	onAddTags,
	onRequestPdfUpload,
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
					onMouseEnter={() => onHoverPaper?.(p.path ?? null)}
				>
					{visibleColumns.map((col) => (
						<Fragment key={col.key}>
							{COLUMN_META[col.key].render(p, cellCtx)}
						</Fragment>
					))}
				</tr>
			</ContextMenuTrigger>
			<ContextMenuContent className="w-44">
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
						<ContextMenuItem
							className="text-destructive focus:text-destructive"
							onSelect={() => void trashPathsAndNotify([paperAbsPath])}
						>
							<Trash2 className="size-3.5" />
							{ctx.t("fileTree.delete")}
						</ContextMenuItem>
					</>
				) : null}
			</ContextMenuContent>
		</ContextMenu>
	);
});
