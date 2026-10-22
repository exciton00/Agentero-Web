/**
 * Shared "task burst" window for watcher / lifecycle driven UI refreshes.
 *
 * One server task (import, asset download, body refs parse, skill install …)
 * materializes files in stages. Several independent sources then ask for a
 * refresh while the pipeline is still running: the merged watcher payloads, the
 * `paper:imported` / `paper:assets-ready` lifecycle events and `job:changed`
 * completions. Each scheduler has its own debounce, so a pipeline spanning a
 * few seconds used to fire 3-4 full tree / catalog / wiki refreshes in a row —
 * the "auto refresh" users see long after the task itself looks finished.
 *
 * A burst is *announced* by the watcher gate when it delivers a multi-file
 * payload. While the burst window is open, the three schedulers stretch their
 * debounce to [`BURST_HOLD_MS`], so every request that lands inside the window
 * feeds the same trailing refresh instead of starting another one. A long
 * running task still refreshes every [`BURST_MAX_MS`] so the UI never freezes
 * until the task ends.
 *
 * A single external edit never announces a burst: its refresh keeps the
 * surface's own debounce (tree 400 ms / library 500 ms / wiki 900 ms).
 */

/** Trailing window a burst keeps later refresh requests in the same pass. */
export const BURST_HOLD_MS = 1500;
/** Longest a single burst may defer the refresh (progress tick for big tasks). */
export const BURST_MAX_MS = 8000;
/** Short debounce used when a burst has been held for [`BURST_MAX_MS`]. */
const BURST_TICK_MS = 250;

let burstStartedAt = 0;
let burstUntil = 0;

/** Announce a multi-file / task write burst (called by the watcher gate). */
export function markRefreshBurst(now = Date.now()): void {
	// A quiet gap starts a new burst, so the max-hold clock resets.
	if (now >= burstUntil) burstStartedAt = now;
	burstUntil = now + BURST_HOLD_MS;
}

/** True while a task write burst is cooling down. */
export function isRefreshBurst(now = Date.now()): boolean {
	return now < burstUntil;
}

/**
 * Debounce window for one refresh request.
 *
 * `defaultMs` is the caller's own activity-aware delay (see
 * `watchRefreshDelayMs`). It is kept for isolated edits; inside a burst the
 * request waits for the burst to settle so all surfaces refresh once.
 */
export function refreshDelayMs(defaultMs: number, now = Date.now()): number {
	if (!isRefreshBurst(now)) return defaultMs;
	// Very long burst (large download): flush periodically instead of waiting
	// for the whole task to finish, then start a fresh window.
	if (now - burstStartedAt >= BURST_MAX_MS) {
		burstStartedAt = now;
		return Math.min(defaultMs, BURST_TICK_MS);
	}
	return Math.max(defaultMs, BURST_HOLD_MS);
}
