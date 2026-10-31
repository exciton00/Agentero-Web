/**
 * Persist the visible Agent transcript to the Agentero-side cache.
 *
 * Dsh can finish a turn `interrupted` without persisting the final assistant
 * text; the only surviving copy is the streamed transcript in this browser
 * session. This hook mirrors the shared session store into
 * `transcript-cache` with a debounced write per conversation, and flushes
 * synchronously before the page is hidden/unloaded so a refresh right after the
 * answer arrives cannot lose it.
 *
 * The store is never mutated here; persistence is best-effort and must never
 * affect the chat path.
 */

import { useEffect } from "react";
import { agentSessionStore } from "@/lib/agent/agent-session-store";
import {
	createTranscriptPersister,
	flushScheduledTranscriptSaves,
} from "@/lib/agent/transcript-cache";

export function useAgentTranscriptPersistence(): void {
	useEffect(() => {
		const persister = createTranscriptPersister();
		const unsubscribe = agentSessionStore.subscribe((state) => {
			persister.sync(state.sessions);
		});
		persister.sync(agentSessionStore.getState().sessions);

		// localStorage writes are synchronous, so a pagehide flush is safe and
		// survives a refresh immediately after the streamed answer.
		const flush = () => flushScheduledTranscriptSaves();
		const onVisibilityChange = () => {
			if (document.visibilityState === "hidden") flush();
		};
		window.addEventListener("pagehide", flush);
		window.addEventListener("beforeunload", flush);
		document.addEventListener("visibilitychange", onVisibilityChange);

		return () => {
			unsubscribe();
			window.removeEventListener("pagehide", flush);
			window.removeEventListener("beforeunload", flush);
			document.removeEventListener("visibilitychange", onVisibilityChange);
			// Unmounting (panel closed / vault switch) must not drop the last turn.
			flush();
		};
	}, []);
}
