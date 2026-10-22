/**
 * Activity-gated, burst-coalesced `vault:file-changed` subscription.
 *
 * Three jobs:
 * 1. While the shell is hidden or unfocused, payloads are buffered. Returning
 *    to the foreground flushes them as one merged delivery so tree / editor /
 *    wiki work does not run against an invisible window.
 * 2. Frames that arrive back-to-back (a task materializing a paper writes many
 *    files; the server watcher may emit several batches before the burst
 *    settles) are merged over a short quiet window, so consumers run their
 *    refresh chain once per burst instead of once per frame. A single external
 *    edit still dispatches after `COALESCE_QUIET_MS`, keeping it timely.
 * 3. Drop the watcher echo of this tab's own writes.
 *
 * Merging keeps every touched path in one payload (`paths` is already the
 * batch shape), collapses duplicate paths, and promotes a mixed-kind burst to
 * `"other"` so structural refreshes are never lost. Trustworthy rename pairs
 * stay separate payloads: consumers repair internal links from one `{from,to}`
 * pair.
 */

import { events } from "@/lib/core/bindings";
import { listenEventSafe } from "@/lib/core/tauri-events";
import type { VaultFileChangedPayload } from "@/lib/vault/fs-watch";
import { markRefreshBurst } from "@/lib/vault/refresh-burst";
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

/** Quiet window that closes one watcher burst before consumers run. */
const COALESCE_QUIET_MS = 180;
/**
 * Upper bound on how long a continuously written burst is held: a long task
 * writing files must not postpone the refresh indefinitely. Frames that keep a
 * burst open are merged until this cap, then dispatched and the next window
 * starts.
 */
const COALESCE_MAX_MS = 1200;

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
 * Merge a burst of watcher payloads into the fewest equivalent deliveries:
 * verified rename pairs stay as they are, everything else becomes one payload
 * with the union of touched paths.
 */
export function mergeVaultFileChangedPayloads(
	payloads: readonly VaultFileChangedPayload[],
): VaultFileChangedPayload[] {
	const renamed: VaultFileChangedPayload[] = [];
	const paths: string[] = [];
	const seenPaths = new Set<string>();
	const kinds = new Set<string>();

	for (const payload of payloads) {
		if (payload.rename) {
			renamed.push(payload);
			continue;
		}
		kinds.add(payload.kind);
		for (const path of payload.paths) {
			const key = normalizeChangedPath(path);
			if (seenPaths.has(key)) continue;
			seenPaths.add(key);
			paths.push(path);
		}
	}

	const merged: VaultFileChangedPayload[] = [...renamed];
	if (paths.length > 0) {
		merged.push({
			paths,
			kind: kinds.size === 1 ? [...kinds][0] : "other",
		});
	}
	return merged;
}

/**
 * Listen for Host vault file changes with activity gating and burst coalescing.
 * The disposer drops any buffered or coalesced payloads that were not delivered
 * yet.
 */
export function listenVaultFileChangedGated(
	handler: VaultFileChangedHandler,
): () => void {
	/** Payloads held while the shell is inactive (delivered on return). */
	const buffer: VaultFileChangedPayload[] = [];
	/** Payloads of the burst currently being coalesced while active. */
	const pending: VaultFileChangedPayload[] = [];
	/** Serial delivery chain: handlers may be async and must stay in order. */
	let delivery: Promise<void> = Promise.resolve();
	let pendingTimer: ReturnType<typeof setTimeout> | null = null;
	/** When the oldest still-pending payload arrived (burst max-wait anchor). */
	let pendingSince = 0;
	let disposed = false;

	const deliverMerged = (payloads: readonly VaultFileChangedPayload[]) => {
		if (disposed || payloads.length === 0) return;
		const merged = mergeVaultFileChangedPayloads(payloads);
		// A multi-file payload is a task materializing files, not one external
		// edit: announce the burst so the tree / library / wiki schedulers fold
		// their refreshes into one trailing pass instead of one per stage.
		if (
			payloads.length > 1 ||
			merged.some((payload) => payload.paths.length > 1)
		) {
			markRefreshBurst();
		}
		delivery = delivery
			.then(async () => {
				for (const payload of merged) {
					if (disposed) return;
					await handler(payload);
				}
			})
			.catch(() => {
				// A consumer failure must not wedge the delivery chain.
			});
	};

	const flushPending = () => {
		if (pendingTimer !== null) {
			clearTimeout(pendingTimer);
			pendingTimer = null;
		}
		if (disposed || pending.length === 0) return;
		// A hidden shell must not run refresh work: keep the burst for the
		// foreground flush instead of delivering it now.
		if (!isShellActive()) {
			buffer.push(...pending.splice(0));
			return;
		}
		const burst = pending.splice(0);
		pendingSince = 0;
		deliverMerged(burst);
	};

	const schedulePendingFlush = () => {
		if (disposed) return;
		const now = Date.now();
		// Coalesce with a quiet window, but never wait past the burst cap.
		const remainingCap = COALESCE_MAX_MS - (now - pendingSince);
		const delay = Math.max(0, Math.min(COALESCE_QUIET_MS, remainingCap));
		if (pendingTimer !== null) clearTimeout(pendingTimer);
		pendingTimer = setTimeout(() => {
			pendingTimer = null;
			flushPending();
		}, delay);
	};

	const unlisten = listenEventSafe(events.vaultFileChanged, (payload) => {
		if (disposed) return;
		// Drop the watcher echo of this tab's own writes: refreshing the tree /
		// wiki / editor for them is redundant work (and, for the tree, a periodic
		// visible flash during autosave).
		if (isSelfAuthoredPayload(payload)) return;
		if (!isShellActive()) {
			buffer.push(payload);
			return;
		}
		if (pending.length === 0) pendingSince = Date.now();
		pending.push(payload);
		schedulePendingFlush();
	});

	const unsubActivity = subscribeShellActivity((active) => {
		if (disposed) return;
		if (!active) {
			// Freeze the burst: move coalesced payloads behind the inactive gate.
			if (pendingTimer !== null) {
				clearTimeout(pendingTimer);
				pendingTimer = null;
			}
			if (pending.length > 0) buffer.push(...pending.splice(0));
			pendingSince = 0;
			return;
		}
		const backlog = buffer.splice(0);
		deliverMerged(backlog);
		flushPending();
	});

	return () => {
		disposed = true;
		buffer.length = 0;
		pending.length = 0;
		if (pendingTimer !== null) {
			clearTimeout(pendingTimer);
			pendingTimer = null;
		}
		unlisten();
		unsubActivity();
	};
}
