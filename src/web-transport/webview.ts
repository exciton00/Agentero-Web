/**
 * Browser replacement for `@tauri-apps/api/webview`.
 *
 * Only the `DragDropEvent` type and the current-webview object are used by the
 * frontend. Browsers expose dropped files through the HTML5 `DataTransfer`
 * API (no absolute paths), so native drop events never fire here — the app's
 * HTML5 drag overlay covers the browser case.
 */

import { getWindowLabel } from "./label";
import { WebWindow } from "./window";

export type PhysicalPosition = {
	x: number;
	y: number;
};

export type DragDropEvent =
	| { type: "enter"; paths: string[]; position: PhysicalPosition }
	| { type: "over"; position: PhysicalPosition }
	| { type: "drop"; paths: string[]; position: PhysicalPosition }
	| { type: "leave" };

/** Tauri `Webview` surface implemented by the web host (same tab as window). */
export class Webview extends WebWindow {}

let current: Webview | null = null;

/** The webview backing this browser tab. */
export function getCurrentWebview(): Webview {
	current ??= new Webview(getWindowLabel());
	return current;
}
