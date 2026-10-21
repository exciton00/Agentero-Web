/**
 * Browser replacement for `@tauri-apps/api/window`.
 *
 * A browser tab is the web host's "window": it has a stable label (see
 * `label.ts`) and the event surface the app uses, but none of the native window
 * chrome (focus, fullscreen, drag-drop paths, theme) — those are either mapped
 * onto the closest browser API or become no-ops.
 *
 * The shim is deliberately minimal: it implements the methods the frontend
 * actually calls (grep `getCurrentWindow()`), so desktop-only features degrade
 * quietly instead of throwing.
 */

import type { EventCallback, UnlistenFn } from "./event";
import { emit, listen, once } from "./event";
import { getWindowLabel } from "./label";

export type Theme = "light" | "dark";

/** Tauri `Window` surface implemented by the web host. */
export class WebWindow {
	/** Per-tab label (`tab-…`), used for targeted events. */
	readonly label: string;

	constructor(label?: string) {
		this.label = label ?? getWindowLabel();
	}

	/** Subscribe to an event delivered to this tab. */
	listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
		return listen(event, handler);
	}

	/** Subscribe to the next occurrence of an event. */
	once<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
		return once(event, handler);
	}

	/** Broadcast an event to every open tab. */
	emit<T>(event: string, payload?: T): Promise<void> {
		return emit(event, payload);
	}

	/** Emit an event to one tab (by label or window shim). */
	emitTo<T>(
		target: string | WebWindow,
		event: string,
		payload?: T,
	): Promise<void> {
		const label = typeof target === "string" ? target : target.label;
		return emit(event, payload, { target: label });
	}

	/** No native file drag-and-drop exists in the browser (HTML5 DnD is used). */
	onDragDropEvent(_handler: (event: unknown) => void): Promise<UnlistenFn> {
		return Promise.resolve(() => undefined);
	}

	/** Resize events are not mirrored; the browser layout reflows natively. */
	onResized(_handler: (event: unknown) => void): Promise<UnlistenFn> {
		return Promise.resolve(() => undefined);
	}

	/** Best-effort tab close (`window.close()` only works for script-opened tabs). */
	async close(): Promise<void> {
		try {
			window.close();
		} catch {
			// Browsers may refuse; the tab then stays open — acceptable.
		}
	}

	/** Alias of {@link close} kept for Tauri parity. */
	async destroy(): Promise<void> {
		await this.close();
	}

	async show(): Promise<void> {
		// Already visible: no native window to reveal.
	}

	async hide(): Promise<void> {
		// Cannot hide a browser tab.
	}

	async isVisible(): Promise<boolean> {
		return true;
	}

	async setFocus(): Promise<void> {
		try {
			window.focus();
		} catch {
			// Ignore focus restrictions.
		}
	}

	async setTitle(title: string): Promise<void> {
		try {
			document.title = title;
		} catch {
			// Ignore.
		}
	}

	/** Theme is applied to the document by the app itself; no native chrome. */
	async setTheme(_theme: Theme | null): Promise<void> {
		// No-op: the browser has no title bar to theme.
	}

	async isMaximized(): Promise<boolean> {
		return Boolean(
			document.fullscreenElement ?? document.documentElement.dataset.maximized,
		);
	}

	async maximize(): Promise<void> {
		try {
			await document.documentElement.requestFullscreen();
		} catch {
			// Fullscreen requires a user gesture / may be blocked.
		}
	}

	async unmaximize(): Promise<void> {
		try {
			if (document.fullscreenElement) await document.exitFullscreen();
		} catch {
			// Ignore.
		}
	}

	async minimize(): Promise<void> {
		// Cannot minimize a browser tab.
	}

	async isFullscreen(): Promise<boolean> {
		return Boolean(document.fullscreenElement);
	}

	async setFullscreen(fullscreen: boolean): Promise<void> {
		try {
			if (fullscreen) {
				if (!document.fullscreenElement) {
					await document.documentElement.requestFullscreen();
				}
			} else if (document.fullscreenElement) {
				await document.exitFullscreen();
			}
		} catch {
			// Fullscreen may be blocked; callers re-check with isFullscreen().
		}
	}

	async setDecorations(_decorations: boolean): Promise<void> {
		// No native decorations.
	}
}

let current: WebWindow | null = null;

/** The window for this browser tab (one per tab, stable across calls). */
export function getCurrentWindow(): WebWindow {
	current ??= new WebWindow();
	return current;
}
