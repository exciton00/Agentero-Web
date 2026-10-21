/**
 * Browser replacement for `@tauri-apps/plugin-opener`.
 *
 * - `openUrl`: opens the URL in a new browser tab (the user is already in a
 *   browser, so "system browser" and "this browser" coincide).
 * - `openPath`: serves the Vault file through `/api/file` in a new tab
 *   (PDFs render inline; other files download).
 * - `revealItemInDir`: there is no OS file manager to reveal into — no-op.
 */

function openInTab(url: string): void {
	if (typeof window === "undefined") return;
	try {
		window.open(url, "_blank", "noopener,noreferrer");
	} catch {
		// Popup blocked; the caller's fallback (if any) handles it.
	}
}

/** Open an http(s) URL in a new browser tab. */
export async function openUrl(url: string, _openWith?: string): Promise<void> {
	openInTab(url);
}

/** Open a Vault file (by absolute path) in a new browser tab. */
export async function openPath(path: string): Promise<void> {
	const url = new URL("/api/file", window.location.origin);
	url.searchParams.set("path", path);
	openInTab(url.toString());
}

/** No OS file manager in the browser; desktop-only affordance. */
export async function revealItemInDir(_path: string): Promise<void> {
	// No-op.
}
