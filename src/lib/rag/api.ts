/**
 * RAG control/API layer (headless web host, v2.1 generic naming).
 *
 * Commands are plain `ApiResult<T>` RPCs routed through the same transport the
 * generated bindings use (`@tauri-apps/api/core` → HTTP RPC in web mode).
 * `bindings.ts` is generated from the desktop command list and does not know
 * the `rag_*` commands yet, so this module invokes them by name.
 *
 * Every entry point is a no-op error without a Host transport; callers render
 * "backend not ready" instead of throwing on mount.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ApiResult } from "@/lib/core/bindings";
import { hasHostTransport } from "@/lib/core/tauri";
import { toSafeDisposer } from "@/lib/core/tauri-events";
import type {
	RagClearMappingArgs,
	RagClearMappingResult,
	RagListKbsResult,
	RagReconcileArgs,
	RagReconcileResult,
	RagSearchArgs,
	RagSearchResult,
	RagStatus,
	RagStatusEvent,
	RagSyncCancelResult,
	RagSyncProgress,
	RagSyncStartArgs,
	RagSyncStartResult,
	RagTestResult,
} from "@/lib/rag/types";

export const RAG_NO_HOST_MESSAGE =
	"RAG requires the Agentero Host (web host RPC).";

/** `true` when the Host rejects because the RPC is not implemented yet. */
export function isRagUnavailableError(error: unknown): boolean {
	const message =
		error instanceof Error
			? error.message
			: typeof error === "string"
				? error
				: "";
	return /not implemented|unknown_command|unknown command|not available/i.test(
		message,
	);
}

function apiErrorMessage(error: unknown, fallback: string): string {
	if (typeof error === "string" && error.trim()) return error.trim();
	if (typeof error === "object" && error !== null) {
		const record = error as { message?: unknown; code?: unknown };
		if (typeof record.message === "string" && record.message.trim()) {
			return record.message.trim();
		}
		if (typeof record.code === "string" && record.code.trim()) {
			return record.code.trim();
		}
	}
	return fallback;
}

async function callRag<T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> {
	if (!hasHostTransport()) throw new Error(RAG_NO_HOST_MESSAGE);
	const response = await invoke<ApiResult<T>>(command, args);
	if (!response || typeof response !== "object" || response.ok !== true) {
		throw new Error(
			apiErrorMessage(
				response && typeof response === "object" ? response.error : response,
				`RAG command "${command}" failed`,
			),
		);
	}
	return response.data as T;
}

/** Settings summary: provider, KB info, sync counters, auto-sync, tool exposure. */
export function ragStatus(): Promise<RagStatus> {
	return callRag<RagStatus>("rag_status");
}

/** Probe connection + KB. Omitted fields fall back to the stored settings. */
export function ragTest(args?: {
	baseUrl?: string;
	apiKey?: string;
	knowledgeBaseId?: string;
}): Promise<RagTestResult> {
	return callRag<RagTestResult>("rag_test", args ?? {});
}

/** List knowledge bases visible to the configured credentials. */
export function ragListKbs(args?: {
	baseUrl?: string;
	apiKey?: string;
}): Promise<RagListKbsResult> {
	return callRag<RagListKbsResult>("rag_list_kbs", args ?? {});
}

/** Start a sync run (or one paper when `paper` is set). Progress via events. */
export function ragSyncStart(
	args: RagSyncStartArgs = {},
): Promise<RagSyncStartResult> {
	return callRag<RagSyncStartResult>("rag_sync_start", args);
}

export function ragSyncCancel(): Promise<RagSyncCancelResult> {
	return callRag<RagSyncCancelResult>("rag_sync_cancel", {});
}

/** Rebuild the local mapping from KB metadata (orphan prune opt-in). */
export function ragReconcile(
	args: RagReconcileArgs = {},
): Promise<RagReconcileResult> {
	return callRag<RagReconcileResult>("rag_reconcile", args);
}

/** Drop the local mapping; optionally delete Agentero-managed KB documents. */
export function ragClearMapping(
	args: RagClearMappingArgs = {},
): Promise<RagClearMappingResult> {
	return callRag<RagClearMappingResult>("rag_clear_mapping", args);
}

/** Read-only semantic search over the synced knowledge base. */
export function ragSearch(args: RagSearchArgs): Promise<RagSearchResult> {
	return callRag<RagSearchResult>("rag_search", { ...args });
}

/** Subscribe to live sync progress (`rag:sync`); returns an unlisten. */
export function listenRagSync(
	handler: (payload: RagSyncProgress) => void,
): () => void {
	if (!hasHostTransport()) return () => undefined;
	return toSafeDisposer(
		listen<RagSyncProgress>("rag:sync", (event) => handler(event.payload)),
	);
}

/** Subscribe to status changes (`rag:changed`); returns an unlisten. */
export function listenRagChanged(
	handler: (payload: RagStatusEvent) => void,
): () => void {
	if (!hasHostTransport()) return () => undefined;
	return toSafeDisposer(
		listen<RagStatusEvent>("rag:changed", (event) => handler(event.payload)),
	);
}
