import { describe, expect, it } from "vitest";
import type {
	AnnotationRefKind,
	PaperAnnotationSummary,
} from "@/lib/pdf/annotation-ref";
import {
	highlightRowsFromHighlights,
	highlightRowsFromSummaries,
} from "@/lib/pdf/highlight/panel-rows";
import type { PdfHighlight } from "@/lib/pdf/highlight/types";

function highlight(overrides: Partial<PdfHighlight>): PdfHighlight {
	return {
		version: 1,
		kind: "highlight",
		id: "h1",
		paperPath: "papers/p",
		createdAt: "2026-01-01T00:00:00.000Z",
		updatedAt: "2026-01-01T00:00:00.000Z",
		page: 1,
		rects: [{ x: 0.1, y: 0.1, w: 0.2, h: 0.02 }],
		quote: "quote",
		...overrides,
	};
}

function summary(
	overrides: Partial<PaperAnnotationSummary> & {
		kind?: AnnotationRefKind;
	},
): PaperAnnotationSummary {
	return {
		kind: "highlight",
		id: "s1",
		page: 1,
		quote: "quote",
		comment: "",
		preview: "quote",
		...overrides,
	};
}

describe("annotations panel highlight rows", () => {
	it("returns no rows without live highlights", () => {
		expect(highlightRowsFromHighlights(undefined, null)).toEqual([]);
		expect(highlightRowsFromHighlights([], "Paper")).toEqual([]);
	});

	it("sorts live highlights by page then reading order", () => {
		const rows = highlightRowsFromHighlights(
			[
				highlight({
					id: "page2",
					page: 2,
					rects: [{ x: 0, y: 0.1, w: 1, h: 1 }],
				}),
				highlight({
					id: "page1-late",
					page: 1,
					rects: [{ x: 0, y: 0.8, w: 1, h: 1 }],
				}),
				highlight({
					id: "page1-early",
					page: 1,
					rects: [{ x: 0, y: 0.2, w: 1, h: 1 }],
				}),
			],
			null,
		);
		expect(rows.map((r) => r.id)).toEqual([
			"page1-early",
			"page1-late",
			"page2",
		]);
	});

	it("maps quote, note, and palette color with a default", () => {
		const [row] = highlightRowsFromHighlights(
			[
				highlight({
					id: "a",
					quote: "selected text",
					comment: "my note",
					color: "purple",
				}),
			],
			null,
		);
		expect(row).toMatchObject({
			id: "a",
			page: 1,
			quote: "selected text",
			comment: "my note",
			color: "purple",
		});
		// A hex color outside the palette keys falls back to the default.
		const fallback = highlightRowsFromHighlights(
			[highlight({ color: "#7dd3fc" })],
			null,
		)[0];
		expect(fallback.color).toBe("yellow");
		// Notes are optional; the panel renders the quote when the note is empty.
		const plain = highlightRowsFromHighlights([highlight({})], null)[0];
		expect(plain.comment).toBe("");
	});

	it("builds the copy alias from the paper title and note", () => {
		const [row] = highlightRowsFromHighlights(
			[highlight({ quote: "quoted words", comment: "a note" })],
			"Paper Title",
		);
		expect(row.linkAlias).toBe("Paper Title·a note");
	});

	it("sorts and filters disk summaries to highlight rows", () => {
		const rows = highlightRowsFromSummaries(
			[
				summary({ id: "visual-1", kind: "visual", page: 1, preview: "area" }),
				summary({
					id: "page3",
					page: 3,
					quote: "third page",
					comment: "note 3",
					color: "blue",
				}),
				summary({ id: "page1", page: 1, quote: "first page" }),
			],
			null,
		);
		expect(rows.map((r) => r.id)).toEqual(["page1", "page3"]);
		expect(rows[1]).toMatchObject({
			page: 3,
			quote: "third page",
			comment: "note 3",
			color: "blue",
		});
		expect(highlightRowsFromSummaries(undefined, null)).toEqual([]);
	});
});
