/**
 * Row interactions for the library table.
 *
 * - Single-click selects the row (Paper Info follows the selection).
 * - Double-click the row opens the paper (`detail > 1` arrives on the second click).
 * - Press and hold a copyable cell (~550ms) copies that field; the release
 *   click is swallowed so the row does not open. Double-click no longer copies.
 * - Direct open (context menu / Enter) opens immediately.
 *
 * The long press deliberately never calls `preventDefault()` on pointer down:
 * native text selection, Ctrl/Cmd+C and the browser context menu stay intact.
 */
import type {
	MouseEvent as ReactMouseEvent,
	PointerEvent as ReactPointerEvent,
} from "react";
import { useCallback, useEffect, useRef } from "react";
import type { CellT } from "@/components/library/library-row-utils";
import { copyTextToClipboard } from "@/lib/core/clipboard";
import type { PaperMetadata } from "@/lib/paper";

/** How long the pointer must stay down before a cell copy fires. */
export const CELL_COPY_LONG_PRESS_MS = 550;
/** Movement (px) that turns the press into a drag and cancels the copy. */
const LONG_PRESS_MOVE_TOLERANCE_PX = 6;

export type CellLongPressHandlers = {
	onPointerDown: (e: ReactPointerEvent<HTMLElement>) => void;
	onPointerMove: (e: ReactPointerEvent<HTMLElement>) => void;
	onPointerUp: (e: ReactPointerEvent<HTMLElement>) => void;
	onPointerLeave: () => void;
	onPointerCancel: () => void;
	/** Swallows the click that follows a fired long press. */
	onClick: (e: ReactMouseEvent<HTMLElement>) => void;
};

/**
 * Long-press-to-copy for one library cell. Returns pointer/click handlers that
 * are spread onto the cell's `<button>`.
 *
 * Cancellation: moving more than {@link LONG_PRESS_MOVE_TOLERANCE_PX}, the
 * pointer leaving the cell, scrolling, `pointercancel`, or unmounting.
 */
export function useCellLongPress({
	copyText,
	label,
	onCopy,
}: {
	copyText: string | null | undefined;
	label: string;
	onCopy: (text: string | null | undefined, label: string) => void;
}): CellLongPressHandlers {
	const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const originRef = useRef<{ x: number; y: number } | null>(null);
	const pointerIdRef = useRef<number | null>(null);
	/** Set when the timer fired; clears itself on the following click. */
	const suppressClickRef = useRef(false);

	const stopTimer = useCallback(() => {
		if (timerRef.current !== null) {
			clearTimeout(timerRef.current);
			timerRef.current = null;
		}
	}, []);

	/** Full reset: pending timer, origin and the scroll/watch listeners. */
	const cancel = useCallback(() => {
		stopTimer();
		originRef.current = null;
		pointerIdRef.current = null;
	}, [stopTimer]);

	const onScroll = useCallback(() => {
		cancel();
	}, [cancel]);

	/** While a press is pending, any scroll (bubbling captured at document) cancels it. */
	const watchScroll = useCallback(() => {
		document.addEventListener("scroll", onScroll, {
			capture: true,
			passive: true,
		});
	}, [onScroll]);

	const unwatchScroll = useCallback(() => {
		document.removeEventListener("scroll", onScroll, { capture: true });
	}, [onScroll]);

	const onPointerDown = useCallback(
		(e: ReactPointerEvent<HTMLElement>) => {
			if (!e.isPrimary || e.button !== 0) return;
			if (!copyText?.trim()) return;
			// A fresh press always starts clean (a previous long press may have
			// ended outside the cell, leaving the suppress flag set).
			suppressClickRef.current = false;
			cancel();
			pointerIdRef.current = e.pointerId;
			originRef.current = { x: e.clientX, y: e.clientY };
			watchScroll();
			timerRef.current = setTimeout(() => {
				timerRef.current = null;
				unwatchScroll();
				originRef.current = null;
				pointerIdRef.current = null;
				suppressClickRef.current = true;
				onCopy(copyText, label);
			}, CELL_COPY_LONG_PRESS_MS);
		},
		[cancel, copyText, label, onCopy, unwatchScroll, watchScroll],
	);

	const onPointerMove = useCallback(
		(e: ReactPointerEvent<HTMLElement>) => {
			const origin = originRef.current;
			if (!origin || pointerIdRef.current !== e.pointerId) return;
			const dx = e.clientX - origin.x;
			const dy = e.clientY - origin.y;
			if (dx * dx + dy * dy > LONG_PRESS_MOVE_TOLERANCE_PX ** 2) {
				cancel();
				unwatchScroll();
			}
		},
		[cancel, unwatchScroll],
	);

	const endPress = useCallback(() => {
		stopTimer();
		unwatchScroll();
		originRef.current = null;
		pointerIdRef.current = null;
	}, [stopTimer, unwatchScroll]);

	const onPointerUp = useCallback(
		(e: ReactPointerEvent<HTMLElement>) => {
			if (
				pointerIdRef.current !== null &&
				e.pointerId !== pointerIdRef.current
			) {
				return;
			}
			endPress();
		},
		[endPress],
	);

	const onPointerLeave = useCallback(() => {
		// Leaving the cell cancels a pending copy; a fired long press keeps the
		// suppress flag until the next press starts.
		cancel();
		unwatchScroll();
	}, [cancel, unwatchScroll]);

	const onPointerCancel = useCallback(() => {
		cancel();
		unwatchScroll();
	}, [cancel, unwatchScroll]);

	const onClick = useCallback((e: ReactMouseEvent<HTMLElement>) => {
		if (!suppressClickRef.current) return;
		suppressClickRef.current = false;
		// Do not let the row's click handler select or open the paper.
		e.preventDefault();
		e.stopPropagation();
	}, []);

	// Unmount / row virtualization must not leave a timer or document listener.
	useEffect(() => {
		return () => {
			if (timerRef.current !== null) {
				clearTimeout(timerRef.current);
				timerRef.current = null;
			}
			document.removeEventListener("scroll", onScroll, { capture: true });
		};
	}, [onScroll]);

	return {
		onPointerDown,
		onPointerMove,
		onPointerUp,
		onPointerLeave,
		onPointerCancel,
		onClick,
	};
}

export function useCellCopy({
	t,
	onOpenPaper,
	onSelectPaper,
}: {
	t: CellT;
	onOpenPaper: (paper: PaperMetadata) => void;
	onSelectPaper?: (paper: PaperMetadata) => void;
}) {
	/** Copy one field (long press / keyboard path); skip empty values. */
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

	/** Copy immediately (cell long-press handler). */
	const onCellCopy = useCallback(
		(text: string | null | undefined, label: string) => {
			void copyField(text, label);
		},
		[copyField],
	);

	/**
	 * Row click: the first click selects, the second (double-click) opens the
	 * paper. Copyable cells swallow the click that follows a long press.
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
