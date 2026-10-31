export type PdfPageBounds = { top: number; bottom: number };

export function clampPageScroll(
	bounds: PdfPageBounds,
	viewportHeight: number,
	top: number,
): number {
	return Math.min(
		Math.max(bounds.top, bounds.bottom - viewportHeight),
		Math.max(bounds.top, top),
	);
}

/** Scroll inside an enlarged page before turning. Never carry wheel overshoot
 * into the next page, and never move outside the first/last document page.
 */
export function pageTurnTarget(
	bounds: PdfPageBounds,
	scrollTop: number,
	viewportHeight: number,
	delta: number,
): { top: number } | { turn: -1 | 1 } {
	const end = Math.max(bounds.top, bounds.bottom - viewportHeight);
	if (delta > 0 && scrollTop >= end - 1) return { turn: 1 };
	if (delta < 0 && scrollTop <= bounds.top + 1) return { turn: -1 };
	return { top: clampPageScroll(bounds, viewportHeight, scrollTop + delta) };
}

/** A trackpad gesture (including its momentum tail) turns at most one page. */
export function createPageTurnGesture() {
	let lastEvent = -Infinity;
	let turned = false;
	return {
		accept(now: number) {
			if (now - lastEvent > 180) turned = false;
			lastEvent = now;
			return !turned;
		},
		markTurned() {
			turned = true;
		},
	};
}
