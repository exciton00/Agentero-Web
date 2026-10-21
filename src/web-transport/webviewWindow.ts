/**
 * Browser replacement for `@tauri-apps/api/webviewWindow`.
 *
 * Desktop feature/settings windows are separate webviews. A browser tab cannot
 * spawn native windows, so `WebviewWindow.getByLabel` always reports "not
 * found" (callers then fall back to the in-tab surface or surface a Host
 * error), and the current webview window is the current tab.
 */

import { getWindowLabel } from "./label";
import { WebWindow } from "./window";

/** Tauri `WebviewWindow` surface implemented by the web host. */
export class WebviewWindow extends WebWindow {
	/** No pop-out windows exist in the browser. */
	static getByLabel(_label: string): Promise<WebviewWindow | null> {
		return Promise.resolve(null);
	}

	/** The only "window" is the current tab. */
	static getCurrent(): WebviewWindow {
		return new WebviewWindow(getWindowLabel());
	}

	static getAll(): Promise<WebviewWindow[]> {
		return Promise.resolve([getCurrentWebviewWindow()]);
	}

	static getFocusedWindow(): Promise<WebviewWindow | null> {
		return Promise.resolve(getCurrentWebviewWindow());
	}
}

let current: WebviewWindow | null = null;

/** The webview window backing this browser tab. */
export function getCurrentWebviewWindow(): WebviewWindow {
	current ??= new WebviewWindow(getWindowLabel());
	return current;
}
