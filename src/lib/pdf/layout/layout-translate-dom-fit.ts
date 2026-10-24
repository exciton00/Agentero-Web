/**
 * Browser measurement for a translated paragraph.
 *
 * The heuristic font size is only a guess. This pass checks the real box and,
 * when the translation overflows, tightens leading and then the font size.
 * Results are cached by box size and text. A frame spends at most a few
 * milliseconds measuring; the rest waits for the next frame, and a page that
 * unmounts during a fast scroll cancels its pending measure.
 */

const LINE_HEIGHT = 1.25;
const FIT_SAFETY = 0.97;
const DOM_FIT_ABSOLUTE_MIN = 1;
const DOM_FIT_EPSILON_PX = 0.5;
const DOM_FIT_LINE_HEIGHTS = [LINE_HEIGHT, 1.2, 1.15, 1.1] as const;
const FRAME_BUDGET_MS = 5;
const CACHE_MAX = 2048;

export type LayoutTranslateDomFit = {
	fontSize: number;
	lineHeight: number;
	overflowWrap: "normal" | "anywhere";
};

export type MeasurableParagraph = {
	textContent: string | null;
	style: {
		fontSize: string;
		lineHeight: string;
		overflowWrap: string;
	};
	scrollHeight: number;
	clientHeight: number;
	scrollWidth: number;
	clientWidth: number;
};

export function layoutTranslateDomFitKey(input: {
	text: string;
	boxWidthPx: number;
	boxHeightPx: number;
	initialFontSize: number;
	isHeading: boolean;
	/**
	 * Settings → Translate body/heading multiplier behind `initialFontSize`.
	 * Folded into the key so a scale change re-measures instead of reusing the
	 * fit measured at the previous preference (they can round to the same px).
	 */
	fontScale?: number;
}): string {
	const width = Math.round(input.boxWidthPx * 2);
	const height = Math.round(input.boxHeightPx * 2);
	const fontSize = Math.round(input.initialFontSize * 100);
	const scale = Math.round((input.fontScale ?? 1) * 100);
	return `${width}x${height}@${fontSize}s${scale}${input.isHeading ? "h" : "b"}\n${input.text}`;
}

const domFitCache = new Map<string, LayoutTranslateDomFit>();

export function readLayoutTranslateDomFit(
	key: string,
): LayoutTranslateDomFit | undefined {
	const hit = domFitCache.get(key);
	if (!hit) return undefined;
	domFitCache.delete(key);
	domFitCache.set(key, hit);
	return hit;
}

export function writeLayoutTranslateDomFit(
	key: string,
	fit: LayoutTranslateDomFit,
): void {
	if (domFitCache.has(key)) domFitCache.delete(key);
	domFitCache.set(key, fit);
	while (domFitCache.size > CACHE_MAX) {
		const oldest = domFitCache.keys().next().value;
		if (oldest === undefined) return;
		domFitCache.delete(oldest);
	}
}

function elementFitsBox(element: MeasurableParagraph): boolean {
	return (
		element.scrollHeight <= element.clientHeight + DOM_FIT_EPSILON_PX &&
		element.scrollWidth <= element.clientWidth + DOM_FIT_EPSILON_PX
	);
}

function applyParagraphMetrics(
	element: MeasurableParagraph,
	fontSize: number,
	lineHeight: number,
): void {
	element.style.fontSize = `${fontSize}px`;
	element.style.lineHeight = String(lineHeight);
}

/**
 * Measure and apply a fit. Returns null when the text is not the committed
 * string, or when even the smallest size with relaxed wrapping still overflows.
 */
export function fitLayoutTranslateParagraph(
	element: MeasurableParagraph,
	input: { text: string; initialFontSize: number; isHeading: boolean },
): LayoutTranslateDomFit | null {
	if (element.textContent !== input.text) return null;

	let lineHeight: number = LINE_HEIGHT;
	let overflowWrap: LayoutTranslateDomFit["overflowWrap"] = "normal";
	for (const candidate of DOM_FIT_LINE_HEIGHTS) {
		applyParagraphMetrics(element, input.initialFontSize, candidate);
		if (elementFitsBox(element)) {
			return {
				fontSize: input.initialFontSize,
				lineHeight: candidate,
				overflowWrap,
			};
		}
		lineHeight = candidate;
	}

	const floor = input.isHeading
		? Math.max(DOM_FIT_ABSOLUTE_MIN, input.initialFontSize * 0.9)
		: DOM_FIT_ABSOLUTE_MIN;
	let lo = floor;
	let hi = input.initialFontSize;
	let best = floor;
	applyParagraphMetrics(element, lo, lineHeight);

	if (!elementFitsBox(element)) {
		element.style.overflowWrap = "anywhere";
		overflowWrap = "anywhere";
		for (const candidate of DOM_FIT_LINE_HEIGHTS) {
			applyParagraphMetrics(element, lo, candidate);
			if (elementFitsBox(element)) {
				lineHeight = candidate;
				break;
			}
		}
		if (!elementFitsBox(element)) return null;
	}

	for (let i = 0; i < 10; i++) {
		const mid = (lo + hi) / 2;
		applyParagraphMetrics(element, mid, lineHeight);
		if (elementFitsBox(element)) {
			best = mid;
			lo = mid;
		} else {
			hi = mid;
		}
	}
	const fontSize = Math.max(floor, best * FIT_SAFETY);
	applyParagraphMetrics(element, fontSize, lineHeight);
	return { fontSize, lineHeight, overflowWrap };
}

const fitQueue: Array<() => void> = [];
let spentMs = 0;
let frameResetPosted = false;

function fitClock(): number {
	return typeof performance !== "undefined" ? performance.now() : 0;
}

function requestFitFrame(callback: () => void): void {
	if (typeof requestAnimationFrame === "function") {
		requestAnimationFrame(() => callback());
		return;
	}
	setTimeout(callback, 0);
}

function drainLayoutTranslateDomFits(): void {
	while (fitQueue.length > 0) {
		if (spentMs >= FRAME_BUDGET_MS) {
			postFitFrameReset();
			return;
		}
		const job = fitQueue.shift();
		if (!job) return;
		const started = fitClock();
		job();
		spentMs += fitClock() - started;
	}
}

function postFitFrameReset(): void {
	if (frameResetPosted) return;
	frameResetPosted = true;
	requestFitFrame(() => {
		frameResetPosted = false;
		spentMs = 0;
		drainLayoutTranslateDomFits();
	});
}

/** Run `job` now if this frame still has measure budget, otherwise on a later frame. */
export function scheduleLayoutTranslateDomFit(job: () => void): () => void {
	fitQueue.push(job);
	postFitFrameReset();
	drainLayoutTranslateDomFits();
	return () => {
		const index = fitQueue.indexOf(job);
		if (index >= 0) fitQueue.splice(index, 1);
	};
}
