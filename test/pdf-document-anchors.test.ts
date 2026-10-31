import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
	documentAnchors,
	sectionNumber,
} from "@/lib/pdf/layout/document-anchors";

import easySteer from "./fixtures/pdf-paragraph-anchors/easysteer-layout.json";
import readingOrderCases from "./fixtures/pdf-paragraph-anchors/reading-order-cases.json";

const fixture = JSON.parse(
	readFileSync(
		join(
			dirname(fileURLToPath(import.meta.url)),
			"fixtures/pdf-paragraph-anchors/layout.json",
		),
		"utf8",
	),
) as { regions: Parameters<typeof documentAnchors>[0] };

describe("sectionNumber", () => {
	it("matches the host's Arabic and Roman markers", () => {
		expect(sectionNumber("2 Method")).toBe("2");
		expect(sectionNumber("2.1 Method")).toBe("2.1");
		expect(sectionNumber("2.Method")).toBe("2");
		expect(sectionNumber("III.Method")).toBe("3");
		expect(sectionNumber("III. Results")).toBe("3");
		expect(sectionNumber("I I I. Method")).toBe("3");
		expect(sectionNumber("I Preliminaries (e.g. notation)")).toBe("1");
		expect(sectionNumber("Introduction")).toBeNull();
		expect(sectionNumber("arXiv:2504.16054v1")).toBeNull();
	});
});

describe("documentAnchors", () => {
	it.each([
		easySteer,
		...readingOrderCases,
	])("repairs column ownership for $source.generatedAt", (layout) => {
		const anchors = documentAnchors(layout.regions);
		expect(
			Object.fromEntries(anchors.map((item) => [item.id, item.anchor])),
		).toEqual(layout.expectedAnchors);
		// Enumeration must not depend on sidecar array order or mutate it.
		expect(documentAnchors([...layout.regions].reverse())).toEqual(anchors);
	});

	it("numbers paragraphs in reading order under each section", () => {
		const anchors = documentAnchors(fixture.regions).map((item) => ({
			id: item.id,
			anchor: item.anchor,
		}));
		expect(anchors).toEqual([
			{ id: "h2", anchor: "#2" },
			{ id: "body-1", anchor: "#2#p1" },
			{ id: "body-2", anchor: "#2#p2" },
			{ id: "h21", anchor: "#2.1" },
			{ id: "body-21", anchor: "#2.1#p1" },
			{ id: "h3", anchor: "#3" },
			{ id: "body-3", anchor: "#3#p1" },
		]);
	});

	it("drops anchors when the same section number occurs twice", () => {
		const anchors = documentAnchors([
			{
				id: "h1",
				pageIndex: 0,
				kind: "header",
				text: "2 Method",
				readingOrder: 0,
			},
			{
				id: "p1",
				pageIndex: 0,
				kind: "text",
				text: "body",
				readingOrder: 1,
			},
			{
				id: "h2",
				pageIndex: 1,
				kind: "header",
				text: "2 Other",
				readingOrder: 0,
			},
		]);
		expect(anchors.every((item) => item.anchor === null)).toBe(true);
	});

	it("ignores headers without a numbered marker and body outside a section", () => {
		const anchors = documentAnchors([
			{ id: "body-0", pageIndex: 0, kind: "text", text: "abstract" },
			{ id: "h", pageIndex: 0, kind: "header", text: "Abstract" },
			{ id: "pre", pageIndex: 0, kind: "text", text: "before 1 Method" },
			{
				id: "h1",
				pageIndex: 1,
				kind: "header",
				text: "1 Method",
				readingOrder: 0,
			},
			{
				id: "body",
				pageIndex: 1,
				kind: "text",
				text: "method body",
				readingOrder: 1,
			},
		]);
		expect(anchors.map((item) => [item.id, item.anchor])).toEqual([
			["h", null],
			["h1", "#1"],
			["body", "#1#p1"],
		]);
	});
});
