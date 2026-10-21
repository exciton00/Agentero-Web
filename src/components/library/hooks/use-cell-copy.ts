/**
 * Row interactions for the library table.
 *
 * - Single-click selects the row (Paper Info follows the selection).
 * - Double-click opens the paper (`detail > 1` arrives on the second click).
 * - Double-click a cell still copies that field (called from the cell renderer).
 * - Direct open (context menu / Enter) opens immediately.
 */
import type { MouseEvent as ReactMouseEvent } from "react";
import { useCallback } from "react";
import type { CellT } from "@/components/library/library-row-utils";
import { copyTextToClipboard } from "@/lib/core/clipboard";
import type { PaperMetadata } from "@/lib/paper";

export function useCellCopy({
	t,
	onOpenPaper,
	onSelectPaper,
}: {
	t: CellT;
	onOpenPaper: (paper: PaperMetadata) => void;
	onSelectPaper?: (paper: PaperMetadata) => void;
}) {
	/** Double-click a cell → copy that field; skip empty values. */
	const copyField = useCallback(
		async (text: string | null | undefined, label: string) => {
			const value = text?.trim();
			if (!value) return;
			await copyTextToClipboard(value, {
				successMessage: t("papersLibrary.copied", { label }),
				errorMessage: t("papersLibrary.copyFailed"),
				successNotify: {
					duration: 1500,
					id: "papers-library-copied",
				},
			});
		},
		[t],
	);

	/** Double-click a cell → copy immediately. */
	const onCellCopy = useCallback(
		(text: string | null | undefined, label: string) => {
			void copyField(text, label);
		},
		[copyField],
	);

	/**
	 * Row click: the first click selects, the second (double-click) opens the
	 * paper. Cell double-click copy handlers run in parallel.
	 */
	const onRowClick = useCallback(
		(e: ReactMouseEvent, paper: PaperMetadata) => {
			if (e.detail > 1) {
				onOpenPaper(paper);
				return;
			}
			onSelectPaper?.(paper);
		},
		[onOpenPaper, onSelectPaper],
	);

	/** Direct open from the context menu / keyboard — no click heuristics. */
	const openPaperFromRow = useCallback(
		(paper: PaperMetadata) => {
			onOpenPaper(paper);
		},
		[onOpenPaper],
	);

	return { onCellCopy, onRowClick, openPaperFromRow };
}
