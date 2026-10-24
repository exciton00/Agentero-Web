/**
 * Rows for the annotations panel's "Highlights & notes" section.
 *
 * The panel is fed from two sources: the live EmbedPDF annotation store while
 * a PDF is mounted, and the on-disk `marks/annotations.json` summaries when
 * it is not. Both paths produce the same sorted view model here so the rail
 * and the pop-out window cannot drift apart.
 */

import {
	annotationSnippet,
	annotationWikilinkAlias,
	type PaperAnnotationSummary,
} from "@/lib/pdf/annotation-ref";
import {
	type HighlightColor,
	normalizeHighlightColor,
} from "@/lib/pdf/highlight/palette";
import type { PdfHighlight } from "@/lib/pdf/highlight/types";

/** One highlight row in the annotations panel. */
export type HighlightPanelRow = {
	id: string;
	/** 1-based page number. */
	page: number;
	quote: string;
	comment: string;
	color: HighlightColor;
	/** Full wikilink alias (`Title·snippet`) when copying. */
	linkAlias?: string | null;
};

/** Page, then reading order inside the page. */
function compareRows(
	a: { page: number; y: number },
	b: { page: number; y: number },
): number {
	return a.page - b.page || a.y - b.y;
}

/** Live highlight view models → sorted panel rows. */
export function highlightRowsFromHighlights(
	highlights: readonly PdfHighlight[] | undefined,
	paperTitle: string | null | undefined,
): HighlightPanelRow[] {
	if (!highlights?.length) return [];
	return highlights
		.map((h) => ({
			row: {
				id: h.id,
				page: h.page,
				quote: h.quote,
				comment: h.comment ?? "",
				color: normalizeHighlightColor(h.color),
				linkAlias: annotationWikilinkAlias(
					paperTitle,
					annotationSnippet({ comment: h.comment, quote: h.quote }),
				),
			},
			page: h.page,
			y: h.rects[0]?.y ?? 0,
		}))
		.sort(compareRows)
		.map(({ row }) => row);
}

/** Disk annotation summaries → sorted panel rows (highlights only). */
export function highlightRowsFromSummaries(
	summaries: readonly PaperAnnotationSummary[] | undefined,
	paperTitle: string | null | undefined,
): HighlightPanelRow[] {
	if (!summaries?.length) return [];
	return summaries
		.filter((s) => s.kind === "highlight")
		.map((s) => ({
			id: s.id,
			page: s.page,
			quote: s.quote,
			comment: s.comment,
			color: normalizeHighlightColor(s.color),
			linkAlias: annotationWikilinkAlias(paperTitle, s.preview),
		}))
		.sort((a, b) => a.page - b.page);
}
