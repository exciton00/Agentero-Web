// @vitest-environment happy-dom
/**
 * Settings → Translate body/heading font multipliers applied to the layout
 * translation overlay: final glyph size per block kind and the heading box
 * expansion that keeps a longer translation inside the source band.
 */

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { LayoutTranslateOverlay } from "@/components/viewer/pdf/layers/layout-translate-overlay";
import type { LayoutTranslateItem } from "@/lib/pdf/layout/types";
import { settingsStore } from "@/lib/settings/react-store";

// React act() needs this flag outside a test renderer.
(
	globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const PAGE_WIDTH = 800;
const PAGE_HEIGHT = 1100;

function setScales(bodyFontScale: number, headingFontScale: number): void {
	const settings = settingsStore.getState();
	settingsStore.setState({
		...settings,
		translate: { ...settings.translate, bodyFontScale, headingFontScale },
	});
}

function item(
	partial: Pick<LayoutTranslateItem, "id" | "kind" | "bbox"> & {
		source: string;
		translated: string;
	},
): LayoutTranslateItem {
	return {
		pageIndex: 0,
		readingOrder: 0,
		status: "done",
		...partial,
	};
}

const heading = item({
	id: "heading",
	kind: "header",
	// Narrow band near the right margin: the translated heading must grow
	// downward (the width expansion path cannot borrow enough space).
	bbox: { x: 0.55, y: 0.08, w: 0.4, h: 0.02 },
	source: "Introduction",
	translated:
		"第一章 引言：研究背景、问题动机、本文的主要贡献以及论文的整体组织结构安排",
});
const body = item({
	id: "body",
	kind: "text",
	bbox: { x: 0.1, y: 0.25, w: 0.8, h: 0.15 },
	source:
		"The quick brown fox jumps over the lazy dog while the readers compare device characteristics across a wide range of operating conditions. ".repeat(
			3,
		),
	translated:
		"本文比较了器件在多种工作条件下的特性，并给出了一个用于分析陷阱效应的电流瞬态方法。".repeat(
			3,
		),
});

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

async function renderOverlay(): Promise<void> {
	await act(async () => {
		root.render(
			createElement(LayoutTranslateOverlay, {
				items: [heading, body],
				pageWidthPx: PAGE_WIDTH,
				pageHeightPx: PAGE_HEIGHT,
			}),
		);
	});
}

function paragraphSize(itemId: string): number {
	const paragraph = container.querySelector<HTMLElement>(
		`p[data-layout-item="${itemId}"]`,
	);
	if (!paragraph) throw new Error(`overlay paragraph ${itemId} not rendered`);
	return Number.parseFloat(paragraph.style.fontSize);
}

/** Glyph-layer box of one block; its height encodes the expanded bbox. */
function glyphLayerHeight(itemId: string): number {
	const paragraph = container.querySelector<HTMLElement>(
		`p[data-layout-item="${itemId}"]`,
	);
	if (!paragraph) throw new Error(`overlay paragraph ${itemId} not rendered`);
	const layer = paragraph.parentElement?.parentElement;
	return Number.parseFloat(layer?.style.height ?? "0");
}

beforeEach(() => {
	setScales(1, 1);
	container = document.createElement("div");
	document.body.appendChild(container);
	root = createRoot(container);
});

afterEach(async () => {
	await act(async () => root.unmount());
	container.remove();
	setScales(1, 1);
});

describe("layout translate font scale", () => {
	it("sizes body and heading glyphs with their own multiplier", async () => {
		await renderOverlay();
		const headingAtDefault = paragraphSize("heading");
		const bodyAtDefault = paragraphSize("body");
		expect(headingAtDefault).toBeGreaterThan(0);
		expect(bodyAtDefault).toBeGreaterThan(0);

		await act(async () => setScales(0.8, 0.6));
		const headingScaled = paragraphSize("heading");
		const bodyScaled = paragraphSize("body");

		expect(headingScaled / headingAtDefault).toBeCloseTo(0.6, 1);
		expect(bodyScaled / bodyAtDefault).toBeCloseTo(0.8, 1);
	});

	it("keeps body text untouched by the heading multiplier", async () => {
		await renderOverlay();
		const bodyAtDefault = paragraphSize("body");
		await act(async () => setScales(1, 0.5));
		expect(paragraphSize("body")).toBeCloseTo(bodyAtDefault, 1);
	});

	it("grows a heading box with the scaled font, not the unscaled one", async () => {
		await renderOverlay();
		const defaultHeight = glyphLayerHeight("heading");
		await act(async () => setScales(1, 0.5));
		const scaledHeight = glyphLayerHeight("heading");
		// A shorter glyph size needs fewer lines / less box growth.
		expect(scaledHeight).toBeLessThan(defaultHeight);
	});
});
