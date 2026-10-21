/**
 * Activity-gated `vault:file-changed` subscription.
 *
 * While the shell is hidden or unfocused, payloads are buffered. Returning to
 * the foreground flushes them in order so tree / editor / wiki work does not
 * run against an invisible window.
 */

import { events } from "@/lib/core/bindings";
import { listenEventSafe } from "@/lib/core/tauri-events";
import type { VaultFileChangedPayload } from "@/lib/vault/fs-watch";
import {
	isShellActive,
	subscribeShellActivity,
} from "@/lib/vault/shell-activity";

export type VaultFileChangedHandler = (
	payload: VaultFileChangedPayload,
) => void | Promise<void>;

/**
 * Self-authored write registry for the web host.
 *
 * The headless host cannot tell our own `/api/file` writes apart from external
 * edits the way the desktop Host can drop echoes of its own transactions: every
 * autosave comes back as `vault:file-changed`, and the shell then re-reads the
 * file and rebuilds the tree / wiki index. With the editor's 500 ms autosave
 * debounce that produced a full file-tree rebuild every couple of seconds while
 * typing (a periodic visual flash). The web transport's plugin-fs shim records
 * every path it just wrote here, and gated listeners drop payloads that only
 * touch those paths.
 */
const SELF_WRITE_TTL_MS = 2500;
const selfWrittenPaths = new Map<string, number>();

function normalizeChangedPath(path: string): string {
	return path.replace(/\\/g, "/").replace(/\/+$/, "");
}

/** Register a path this tab just wrote (called from the web plugin-fs shim). */
export function markSelfAuthoredWrite(path: string): void {
	if (!path) return;
	const now = Date.now();
	selfWrittenPaths.set(normalizeChangedPath(path), now + SELF_WRITE_TTL_MS);
	// Opportunistic prune: the map is tiny (one entry per recent save path).
	if (selfWrittenPaths.size > 64) {
		for (const [key, expiresAt] of selfWrittenPaths) {
			if (expiresAt <= now) selfWrittenPaths.delete(key);
		}
	}
}

/** True when every changed path was written by this tab very recently. */
export function isSelfAuthoredPayload(
	payload: VaultFileChangedPayload,
): boolean {
	if (payload.paths.length === 0) return false;
	const now = Date.now();
	for (const path of payload.paths) {
		const expiresAt = selfWrittenPaths.get(normalizeChangedPath(path));
		if (!expiresAt || expiresAt <= now) return false;
	}
	return true;
}

/**
 * Listen for Host vault file changes, buffering while the shell is inactive.
 * The disposer drops any still-buffered payloads.
 */
export function listenVaultFileChangedGated(
	handler: VaultFileChangedHandler,
): () => void {
	const buffer: VaultFileChangedPayload[] = [];
	let flushing = false;
	let disposed = false;

	const deliver = async (payload: VaultFileChangedPayload) => {
		await handler(payload);
	};

	const flush = () => {
		if (disposed || flushing) return;
		if (!isShellActive() || buffer.length === 0) return;
		flushing = true;
		void (async () => {
			try {
				while (!disposed && buffer.length > 0 && isShellActive()) {
					const next = buffer.shift();
					if (!next) break;
					await deliver(next);
				}
			} finally {
				flushing = false;
				if (!disposed && buffer.length > 0 && isShellActive()) flush();
			}
		})();
	};

	const unlisten = listenEventSafe(events.vaultFileChanged, (payload) => {
		if (disposed) return;
		// Drop the watcher echo of this tab's own writes: refreshing the tree /
		// wiki / editor for them is redundant work (and, for the tree, a periodic
		// visible flash during autosave).
		if (isSelfAuthoredPayload(payload)) return;
		if (!isShellActive() || buffer.length > 0 || flushing) {
			buffer.push(payload);
			if (isShellActive()) flush();
			return;
		}
		void deliver(payload);
	});

	const unsubActivity = subscribeShellActivity((active) => {
		if (active) flush();
	});

	return () => {
		disposed = true;
		buffer.length = 0;
		unlisten();
		unsubActivity();
	};
}
