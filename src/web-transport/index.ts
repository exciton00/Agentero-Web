/**
 * Web host bootstrap (`vite --mode web` only).
 *
 * Everything under `src/web-transport/` is aliased over the `@tauri-apps/*`
 * packages in web builds (see `vite.config.ts`), so the React app keeps its
 * imports unchanged. This entry point:
 *
 *  1. installs a `window.__TAURI_INTERNALS__` compatibility object (for code
 *     that probes the Tauri globals directly, and as the documented transport
 *     seam of the web build);
 *  2. records the per-tab window label used for targeted events;
 *  3. warms the event WebSocket so the tab is connected before the first
 *     listener mounts.
 */

import { ensureSocket } from "./event";
import { getWindowLabel } from "./label";
import { rpc } from "./rpc";

type Callback = (response: unknown) => void;

type TauriInternals = {
	metadata: {
		currentWindow: { label: string };
		currentWebview: { label: string };
	};
	invoke: (command: string, args?: unknown) => Promise<unknown>;
	transformCallback: (callback?: Callback, once?: boolean) => number;
	unregisterCallback: (id: number) => void;
	convertFileSrc: (path: string) => string;
};

type WebHostWindow = Window & {
	__AGENTERO_WEB__?: boolean;
	__TAURI_INTERNALS__?: TauriInternals;
};

const callbacks = new Map<number, { callback: Callback; once: boolean }>();
let nextCallbackId = 1;

function transformCallback(callback?: Callback, once = false): number {
	const id = nextCallbackId;
	nextCallbackId += 1;
	if (callback) callbacks.set(id, { callback, once });
	return id;
}

function unregisterCallback(id: number): void {
	callbacks.delete(id);
}

function convertFileSrc(path: string): string {
	const url = new URL("/api/file", window.location.origin);
	url.searchParams.set("path", path);
	return url.toString();
}

function installInternals(): void {
	const host = window as WebHostWindow;
	const label = getWindowLabel();
	host.__AGENTERO_WEB__ = true;
	host.__TAURI_INTERNALS__ = {
		metadata: {
			currentWindow: { label },
			currentWebview: { label },
		},
		invoke: (command, args) => rpc(command, args),
		transformCallback,
		unregisterCallback,
		convertFileSrc,
	};
}

let installed = false;

/** Install the web transport shim exactly once (idempotent). */
export function installWebTransport(): void {
	if (installed || typeof window === "undefined") return;
	installed = true;
	installInternals();
	ensureSocket();
}

/** `GET /api/config` response (platform layer, see webui.md §2.2). */
type WebHostConfig = {
	platform?: unknown;
	version?: unknown;
	vaultRoot?: unknown;
};

/** Same keys as `src/lib/vault/session.ts` (sessionStorage vs localStorage). */
const VAULT_PATH_STORAGE_KEY = "agentero-vault-path";
const RECENT_VAULTS_STORAGE_KEY = "agentero-recent-vaults";

let bootVaultRoot: string | null = null;

async function fetchHostConfig(): Promise<WebHostConfig | null> {
	try {
		const response = await fetch("/api/config", { cache: "no-store" });
		if (!response.ok) return null;
		return (await response.json()) as WebHostConfig;
	} catch {
		return null;
	}
}

/**
 * Seed the browser with the server's default Vault so the app auto-opens it.
 *
 * A web deployment has no OS directory picker: the Vault is a server-side
 * directory chosen by `AGENTERO_WEB_VAULT`. `GET /api/config` reports it; the
 * frontend's existing "restore last Vault" path (`initVaultStore`) then picks
 * it up from `localStorage`, exactly like a desktop relaunch.
 *
 * Only seeds when no local Vault is stored yet (or the stored one is a stale
 * `remote:` handle). Failures are silent: the app falls back to the welcome
 * screen (server-side directory browsing lands in M1).
 *
 * Must run before `initVaultStore()` in `boot()`.
 */
export async function bootstrapWebVault(): Promise<string | null> {
	if (bootVaultRoot) return bootVaultRoot;
	const config = await fetchHostConfig();
	const serverRoot =
		typeof config?.vaultRoot === "string" ? config.vaultRoot.trim() : "";
	let storedRoot = "";
	try {
		const stored = localStorage.getItem(VAULT_PATH_STORAGE_KEY);
		if (stored) storedRoot = stored.trim();
	} catch {
		// Storage unavailable: nothing to preserve.
	}
	// Remote handles are desktop-only (SSH sessions) — never restore them here.
	if (storedRoot.startsWith("remote:")) storedRoot = "";
	const shouldSeed = !storedRoot && Boolean(serverRoot);
	if (shouldSeed) {
		try {
			localStorage.setItem(VAULT_PATH_STORAGE_KEY, serverRoot);
			localStorage.setItem(
				RECENT_VAULTS_STORAGE_KEY,
				JSON.stringify([serverRoot]),
			);
		} catch {
			// Quota / private mode: still open the server Vault this load.
		}
	}
	bootVaultRoot = storedRoot || serverRoot || null;
	return bootVaultRoot;
}

// Imported from `src/main.tsx` only in web mode; installing on evaluation
// keeps the seam in one place even if another entry point imports it.
installWebTransport();
