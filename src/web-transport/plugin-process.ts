/**
 * Browser replacement for `@tauri-apps/plugin-process`.
 *
 * "Relaunch" becomes a page reload; the browser cannot exit the application
 * (only the tab, and only when script-opened).
 */

/** Reload the page (closest browser equivalent of an app relaunch). */
export async function relaunch(): Promise<void> {
	if (typeof window !== "undefined") window.location.reload();
}

/** Restart alias exposed by the plugin on some targets. */
export async function restart(): Promise<void> {
	await relaunch();
}

/** A browser tab cannot terminate the app; no-op. */
export async function exit(_code?: number): Promise<void> {
	// No-op.
}
