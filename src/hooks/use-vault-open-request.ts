/**
 * Listen for Host vault open requests (CLI deep link / second instance).
 */

import { useEffect } from "react";
import { takePendingVaultOpen } from "@/lib/cli/api";
import { events } from "@/lib/core/bindings";
import { notifyError } from "@/lib/core/notify";
import { hasHostTransport } from "@/lib/core/tauri";
import { listenEventSafe } from "@/lib/core/tauri-events";
import { openLocalVaultPath } from "@/lib/vault/actions";

export function useVaultOpenRequest(): void {
	useEffect(() => {
		if (!hasHostTransport()) return;
		let cancelled = false;
		/** In-flight path so event + pending take do not double-activate. */
		let inflight: string | null = null;
		let generation = 0;

		const handlePath = async (path: string | undefined | null) => {
			const trimmed = path?.trim();
			if (!trimmed || cancelled) return;
			// Host emits and also queues pending; drain + dedupe the concurrent pair.
			if (inflight === trimmed) return;
			const gen = ++generation;
			inflight = trimmed;
			try {
				try {
					await takePendingVaultOpen();
				} catch {
					// Older Host or no pending — fine.
				}
				if (cancelled || gen !== generation) return;
				await openLocalVaultPath(trimmed);
			} finally {
				if (gen === generation) {
					inflight = null;
				}
			}
		};

		const offOpen = listenEventSafe(events.vaultOpenRequest, (payload) => {
			void handlePath(payload?.path);
		});
		const offError = listenEventSafe(events.vaultOpenError, (payload) => {
			if (payload?.message) notifyError(payload.message);
		});

		// Startup race: Host may have queued a path before the listener attached.
		void (async () => {
			try {
				const pending = await takePendingVaultOpen();
				if (!cancelled && pending) {
					await handlePath(pending);
				}
			} catch {
				// Non-fatal when the command is unavailable (older Host).
			}
		})();

		return () => {
			cancelled = true;
			offOpen();
			offError();
		};
	}, []);
}
