/**
 * Translation paragraph sizing and the per-frame DOM measurement budget.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	fitLayoutTranslateParagraph,
	layoutTranslateDomFitKey,
	type MeasurableParagraph,
	scheduleLayoutTranslateDomFit,
} from "@/lib/pdf/layout/layout-translate-dom-fit";

function paragraph(
	text: string,
	box: { scrollHeight?: number; clientHeight?: number } = {},
): MeasurableParagraph {
	return {
		textContent: text,
		style: { fontSize: "", lineHeight: "", overflowWrap: "normal" },
		scrollHeight: box.scrollHeight ?? 10,
		clientHeight: box.clientHeight ?? 100,
		scrollWidth: 10,
		clientWidth: 100,
	};
}

const frames: Array<() => void> = [];
let now = 0;

beforeEach(() => {
	frames.length = 0;
	now = 0;
	vi.spyOn(performance, "now").mockImplementation(() => now);
	vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
		frames.push(() => callback(now));
		return frames.length;
	});
});

afterEach(() => {
	let guard = 0;
	while (frames.length > 0 && guard++ < 20) {
		const batch = frames.splice(0, frames.length);
		for (const run of batch) run();
	}
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe("layout translate DOM fit key", () => {
	it("re-measures when the translated font scale changes", () => {
		const base = {
			text: "标题翻译",
			boxWidthPx: 220,
			boxHeightPx: 24,
			initialFontSize: 12,
			isHeading: true,
		};
		expect(layoutTranslateDomFitKey(base)).toBe(
			layoutTranslateDomFitKey({ ...base, fontScale: 1 }),
		);
		expect(layoutTranslateDomFitKey(base)).not.toBe(
			layoutTranslateDomFitKey({ ...base, fontScale: 0.8 }),
		);
	});
});

describe("layout translate DOM fit", () => {
	it("keeps the initial size when the first leading fits", () => {
		const element = paragraph("你好");
		expect(
			fitLayoutTranslateParagraph(element, {
				text: "你好",
				initialFontSize: 16,
				isHeading: false,
			}),
		).toEqual({ fontSize: 16, lineHeight: 1.25, overflowWrap: "normal" });
	});

	it("returns null when even the floor with relaxed wrapping overflows", () => {
		const element = paragraph("overflow", {
			scrollHeight: 500,
			clientHeight: 10,
		});
		expect(
			fitLayoutTranslateParagraph(element, {
				text: "overflow",
				initialFontSize: 16,
				isHeading: false,
			}),
		).toBeNull();
		expect(element.style.overflowWrap).toBe("anywhere");
	});

	it("moves the next measure to the following frame and drops it on cancel", () => {
		const ran: string[] = [];
		scheduleLayoutTranslateDomFit(() => {
			now += 5;
			ran.push("a");
		});
		const cancel = scheduleLayoutTranslateDomFit(() => {
			ran.push("b");
		});
		expect(ran).toEqual(["a"]);
		cancel();
		frames.shift()?.();
		expect(ran).toEqual(["a"]);

		scheduleLayoutTranslateDomFit(() => {
			now += 5;
			ran.push("c");
		});
		scheduleLayoutTranslateDomFit(() => {
			ran.push("d");
		});
		frames.shift()?.();
		expect(ran).toEqual(["a", "c", "d"]);
	});
});
