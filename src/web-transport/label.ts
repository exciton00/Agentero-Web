/**
 * Per-tab window label for the headless web host.
 *
 * Desktop Tauri assigns every webview a unique label (`main`, `settings`,
 * `feature-agent`, …). In the browser each tab is its own "window", so the
 * label is generated once and kept in `sessionStorage` (which is per-tab and
 * survives reloads, unlike `localStorage` shared by every tab).
 *
 * The label is sent on the event WebSocket (`/api/events?label=…`) and on every
 * RPC call (`X-Agentero-Window`), so the server can route targeted events
 * (`agent:*`, `vault:file-changed`, …) back to the tab that asked for them.
 */

const LABEL_STORAGE_KEY = "agentero-web-tab-label";

let cachedLabel: string | null = null;

function randomSuffix(): string {
	const bytes = new Uint8Array(6);
	if (typeof crypto !== "undefined" && "getRandomValues" in crypto) {
		crypto.getRandomValues(bytes);
	} else {
		for (let i = 0; i < bytes.length; i += 1) {
			bytes[i] = Math.floor(Math.random() * 256);
		}
	}
	let out = "";
	for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
	return out;
}

/** Stable label for this browser tab, e.g. `tab-3f9a1c2b7d04`. */
export function getWindowLabel(): string {
	if (cachedLabel) return cachedLabel;
	try {
		const stored = sessionStorage.getItem(LABEL_STORAGE_KEY);
		if (stored) {
			cachedLabel = stored;
			return stored;
		}
		const label = `tab-${randomSuffix()}`;
		sessionStorage.setItem(LABEL_STORAGE_KEY, label);
		cachedLabel = label;
		return label;
	} catch {
		// Private mode / storage disabled: fall back to a per-page-load label.
		cachedLabel = `tab-${randomSuffix()}`;
		return cachedLabel;
	}
}
