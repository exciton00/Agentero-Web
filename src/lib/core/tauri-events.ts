import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { hasHostTransport } from "@/lib/core/tauri";

export type TauriEventHandler<T> = (payload: T) => void;

/** One generated specta event binding (`events.*` in `@/lib/core/bindings`). */
export type TypedEventBinding<T> = {
	listen: (cb: EventCallback<T>) => Promise<UnlistenFn>;
};

/**
 * Wrap a promise-returning subscribe so its disposer is safe to call at any
 * time, including before the subscribe resolves.
 *
 * Subscribing needs a round-trip, so the naive
 * `let off; void subscribe().then((o) => { off = o }); return () => off?.()`
 * shape leaks the listener forever whenever the caller disposes before that
 * round-trip lands — which StrictMode makes happen on every dev mount.
 * Chaining the disposer off the pending promise is always correct.
 */
export function toSafeDisposer(pending: Promise<() => void>): () => void {
	// Keep a terminal branch so a failed subscribe never surfaces as an
	// unhandled rejection when nobody disposes.
	pending.catch(() => undefined);
	let disposed = false;
	return () => {
		if (disposed) return;
		disposed = true;
		void pending.then((off) => off()).catch(() => undefined);
	};
}

/**
 * Subscribe to a typed specta event binding. No-op without a Host transport
 * (plain browser); both the desktop IPC and the web-host WebSocket implement
 * the same typed event bindings.
 */
export function listenEventSafe<T>(
	event: TypedEventBinding<T>,
	handler: TauriEventHandler<T>,
): () => void {
	if (!hasHostTransport()) return () => undefined;
	return toSafeDisposer(event.listen((e) => handler(e.payload)));
}

/** Emit a global Host wire event. No-op without a Host transport; failures are non-fatal. */
export function broadcastSafe(event: string, payload?: unknown): void {
	if (!hasHostTransport()) return;
	void (async () => {
		try {
			const { emit } = await import("@tauri-apps/api/event");
			await emit(event, payload);
		} catch {
			// non-fatal
		}
	})();
}
