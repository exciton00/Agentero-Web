import { useScrollCapability } from "@embedpdf/plugin-scroll/react";
import { useViewportElement } from "@embedpdf/plugin-viewport/react";
import { useZoomCapability } from "@embedpdf/plugin-zoom/react";
import { useEffect } from "react";
import { useSettings } from "@/hooks/use-app-stores";
import { createPageTurnGesture, pageTurnTarget } from "@/lib/pdf/page-turn";
import { handleScrollSyncIntent } from "@/lib/pdf/scroll-sync";

const EXCLUDED =
	"[data-pdf-chrome], [role='dialog'], input, textarea, select, [contenteditable='true'], button, a";

/** Native continuous scrolling stays untouched outside paginated mode. */
export function PageTurnHandler({ documentId }: { documentId: string }) {
	const enabled = useSettings((s) => s.pdfScrollStrategy === "paginated");
	const viewportRef = useViewportElement();
	const { provides: scrollCapability } = useScrollCapability();
	const { provides: zoomCapability } = useZoomCapability();
	useEffect(() => {
		const viewport = viewportRef?.current;
		if (!enabled || !viewport || !scrollCapability || !zoomCapability) return;
		const scroll = scrollCapability.forDocument(documentId);
		const zoom = zoomCapability.forDocument(documentId);
		const gesture = createPageTurnGesture();
		const turn = (direction: number) => {
			handleScrollSyncIntent(documentId);
			if (direction > 0) scroll.scrollToNextPage("instant");
			else scroll.scrollToPreviousPage("instant");
		};
		const excluded = (event: MouseEvent | KeyboardEvent) =>
			event.defaultPrevented ||
			event.ctrlKey ||
			event.metaKey ||
			event.altKey ||
			(event.target instanceof Element && !!event.target.closest(EXCLUDED));
		const wheel = (event: WheelEvent) => {
			if (excluded(event) || event.shiftKey || !event.deltaY) return;
			if (Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
			const item = scroll
				.getLayout()
				.virtualItems.find((item) =>
					item.pageNumbers.includes(scroll.getCurrentPage()),
				);
			if (!item) return;
			event.preventDefault();
			if (!gesture.accept(event.timeStamp)) return;
			const scale = zoom.getState().currentZoomLevel;
			const delta =
				event.deltaY *
				(event.deltaMode === 1
					? 16
					: event.deltaMode === 2
						? viewport.clientHeight
						: 1);
			const target = pageTurnTarget(
				{ top: item.y * scale, bottom: (item.y + item.height) * scale },
				viewport.scrollTop,
				viewport.clientHeight -
					parseFloat(getComputedStyle(viewport).paddingTop) * 2,
				delta,
			);
			if ("turn" in target) {
				gesture.markTurned();
				turn(target.turn);
			} else {
				handleScrollSyncIntent(documentId);
				viewport.scrollTop = target.top;
			}
		};
		const key = (event: KeyboardEvent) => {
			if (excluded(event)) return;
			if (event.key !== "PageDown" && event.key !== "PageUp") return;
			event.preventDefault();
			turn(event.key === "PageDown" ? 1 : -1);
		};
		viewport.addEventListener("wheel", wheel, { passive: false });
		viewport.addEventListener("keydown", key);
		return () => {
			viewport.removeEventListener("wheel", wheel);
			viewport.removeEventListener("keydown", key);
		};
	}, [enabled, documentId, viewportRef, scrollCapability, zoomCapability]);
	return null;
}
