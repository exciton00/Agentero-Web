/**
 * WeKnora RAG control/API layer (headless web host).
 *
 * Commands are plain `ApiResult<T>` RPCs routed through the same transport the
 * generated bindings use (`@tauri-apps/api/core` → HTTP RPC in web mode).
 * `bindings.ts` is generated from the desktop command list and does not know
 * the `weknora_*` commands yet, so this module invokes them by name.
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
	WeKnoraClearMappingArgs,
	WeKnoraClearMappingResult,
	WeKnoraListKbsResult,
	WeKnoraReconcileArgs,
	WeKnoraReconcileResult,
	WeKnoraStatus,
	WeKnoraStatusEvent,
	WeKnoraSyncCancelResult,
	WeKnoraSyncProgress,
	WeKnoraSyncStartArgs,
	WeKnoraSyncStartResult,
	WeKnoraTestResult,
} from "@/lib/weknora/types";

export const WEKNORA_NO_HOST_MESSAGE =
	"WeKnora requires the Agentero Host (web host RPC).";

/** `true` when the Host rejects because the RPC is not implemented yet. */
export function isWeKnoraUnavailableError(error: unknown): boolean {
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

async function callWeKnora<T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> {
	if (!hasHostTransport()) throw new Error(WEKNORA_NO_HOST_MESSAGE);
	const response = await invoke<ApiResult<T>>(command, args);
	if (!response || typeof response !== "object" || response.ok !== true) {
		throw new Error(
			apiErrorMessage(
				response && typeof response === "object" ? response.error : response,
				`WeKnora command "${command}" failed`,
			),
		);
	}
	return response.data as T;
}

/** Settings summary: KB info, sync counters, auto-sync, tool exposure. */
export function weknoraStatus(): Promise<WeKnoraStatus> {
	return callWeKnora<WeKnoraStatus>("weknora_status");
}

/** Probe connection + KB. Omitted fields fall back to the stored settings. */
export function weknoraTest(args?: {
	baseUrl?: string;
	apiKey?: string;
	knowledgeBaseId?: string;
}): Promise<WeKnoraTestResult> {
	return callWeKnora<WeKnoraTestResult>("weknora_test", args ?? {});
}

/** List knowledge bases visible to the configured credentials. */
export function weknoraListKbs(args?: {
	baseUrl?: string;
	apiKey?: string;
}): Promise<WeKnoraListKbsResult> {
	return callWeKnora<WeKnoraListKbsResult>("weknora_list_kbs", args ?? {});
}

/** Start a sync run (or one paper when `paper` is set). Progress via events. */
export function weknoraSyncStart(
	args: WeKnoraSyncStartArgs = {},
): Promise<WeKnoraSyncStartResult> {
	return callWeKnora<WeKnoraSyncStartResult>("weknora_sync_start", args);
}

export function weknoraSyncCancel(): Promise<WeKnoraSyncCancelResult> {
	return callWeKnora<WeKnoraSyncCancelResult>("weknora_sync_cancel", {});
}

/** Rebuild the local mapping from KB custom metadata (orphan prune opt-in). */
export function weknoraReconcile(
	args: WeKnoraReconcileArgs = {},
): Promise<WeKnoraReconcileResult> {
	return callWeKnora<WeKnoraReconcileResult>("weknora_reconcile", args);
}

/** Drop the local mapping; optionally delete Agentero-managed KB documents. */
export function weknoraClearMapping(
	args: WeKnoraClearMappingArgs = {},
): Promise<WeKnoraClearMappingResult> {
	return callWeKnora<WeKnoraClearMappingResult>("weknora_clear_mapping", args);
}

/** Subscribe to live sync progress (`weknora:sync`); returns an unlisten. */
export function listenWeKnoraSync(
	handler: (payload: WeKnoraSyncProgress) => void,
): () => void {
	if (!hasHostTransport()) return () => undefined;
	return toSafeDisposer(
		listen<WeKnoraSyncProgress>("weknora:sync", (event) =>
			handler(event.payload),
		),
	);
}

/** Subscribe to status changes (`weknora:changed`); returns an unlisten. */
export function listenWeKnoraChanged(
	handler: (payload: WeKnoraStatusEvent) => void,
): () => void {
	if (!hasHostTransport()) return () => undefined;
	return toSafeDisposer(
		listen<WeKnoraStatusEvent>("weknora:changed", (event) =>
			handler(event.payload),
		),
	);
}
