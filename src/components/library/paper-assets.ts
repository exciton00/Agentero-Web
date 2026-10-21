/**
 * Library paper asset status (`paper_assets_status`) and destructive asset
 * actions (`paper_delete_assets`) used by the row context menu.
 *
 * The host RPC is optional while the backend lands: callers get `null` / an
 * error and render grey fallbacks instead of breaking the table.
 */
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import { commands } from "@/lib/core/bindings";
import { callApiResult } from "@/lib/core/ipc";
import { hasHostTransport } from "@/lib/core/tauri";
import { getVaultPath } from "@/lib/vault/store";

/** Per-paper asset flags returned by `paper_assets_status`. */
export type PaperAssetsStatus = {
	/** Vault-relative paper folder path. */
	path: string;
	hasPdf: boolean;
	hasLayout: boolean;
	hasLayoutIndex: boolean;
};

/** What one Library row displays (RPC or fallback from catalog `has_pdf`). */
export type PaperAssetsDisplay = {
	hasPdf: boolean;
	hasLayout: boolean;
};

export type PaperAssetKind = "pdf" | "layout";

type Envelope<T> = {
	ok?: boolean;
	data?: T | null;
	error?: { message?: string } | null;
};

function normalizePath(path: string): string {
	return path.replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
}

function envelopeError(error: unknown, fallback: string): Error {
	if (error instanceof Error) return error;
	const message =
		typeof error === "string"
			? error
			: typeof (error as { message?: unknown })?.message === "string"
				? ((error as { message: string }).message ?? fallback)
				: "";
	return new Error(message.trim() || fallback);
}

/**
 * Read every paper's asset flags in one Host round-trip.
 * Returns `null` when the RPC is unavailable (older host / desktop build).
 */
export async function fetchPaperAssetsStatus(
	vaultPath: string | null,
): Promise<Map<string, PaperAssetsStatus> | null> {
	if (!vaultPath || !hasHostTransport()) return null;
	try {
		const response = await invoke<Envelope<unknown>>("paper_assets_status", {
			args: { vaultPath },
		});
		if (response?.ok !== true) return null;
		const rows = Array.isArray(response.data) ? response.data : [];
		const byPath = new Map<string, PaperAssetsStatus>();
		for (const raw of rows) {
			if (typeof raw !== "object" || raw === null) continue;
			const entry = raw as {
				path?: unknown;
				hasPdf?: unknown;
				hasLayout?: unknown;
				hasLayoutIndex?: unknown;
			};
			if (typeof entry.path !== "string" || !entry.path.trim()) continue;
			byPath.set(normalizePath(entry.path), {
				path: normalizePath(entry.path),
				hasPdf: entry.hasPdf === true,
				hasLayout: entry.hasLayout === true,
				hasLayoutIndex: entry.hasLayoutIndex === true,
			});
		}
		return byPath;
	} catch {
		return null;
	}
}

/**
 * Poll the asset-status RPC for the current vault. `refresh()` re-reads it
 * after uploads / deletes / layout runs.
 */
export function usePaperAssetsStatus(vaultPath: string | null): {
	assetsByPath: Map<string, PaperAssetsStatus>;
	refresh: () => void;
} {
	const [assetsByPath, setAssetsByPath] = useState<
		Map<string, PaperAssetsStatus>
	>(() => new Map());
	const [version, setVersion] = useState(0);
	const refresh = useCallback(() => setVersion((value) => value + 1), []);

	// `version` is a manual refresh token (incremented by `refresh()`).
	// biome-ignore lint/correctness/useExhaustiveDependencies: version is a refresh token
	useEffect(() => {
		let cancelled = false;
		void fetchPaperAssetsStatus(vaultPath).then((status) => {
			if (!cancelled) setAssetsByPath(status ?? new Map());
		});
		return () => {
			cancelled = true;
		};
	}, [vaultPath, version]);

	return { assetsByPath, refresh };
}

/**
 * Delete the PDF or the layout-analysis files of one paper.
 * Resolves with the removed absolute paths; rejects with the host message.
 */
export async function deletePaperAssets(opts: {
	vaultPath: string;
	paperPath: string;
	kind: PaperAssetKind;
}): Promise<string[]> {
	const { vaultPath, paperPath, kind } = opts;
	const fallback = `delete ${kind} assets failed`;
	try {
		const response = await invoke<Envelope<{ removed?: unknown }>>(
			"paper_delete_assets",
			{ args: { vaultPath, paperPath, kind } },
		);
		if (response?.ok !== true) {
			throw envelopeError(response?.error, fallback);
		}
		const removed = response.data?.removed;
		return Array.isArray(removed)
			? removed.filter((path): path is string => typeof path === "string")
			: [];
	} catch (error) {
		throw envelopeError(error, fallback);
	}
}

/**
 * Force a fresh layout analysis for one paper (overwrites existing results).
 * Reuses the JobCenter renderer offer with `force: true`.
 */
export async function forceReparsePaperLayout(
	paperAbsPath: string,
): Promise<void> {
	const vaultPath = getVaultPath();
	if (!vaultPath) throw new Error("no vault is open");
	const vaultNorm = vaultPath.replace(/\\/g, "/").replace(/\/+$/, "");
	const paperNorm = paperAbsPath.replace(/\\/g, "/").replace(/\/+$/, "");
	if (!paperNorm.startsWith(`${vaultNorm}/`)) {
		throw new Error("paper is outside the vault");
	}
	const paperRelPath = paperNorm.slice(vaultNorm.length).replace(/^\/+/, "");
	await callApiResult(
		() =>
			commands.jobLayoutAnalyzeEnqueue({
				vaultPath,
				path: paperRelPath,
				lane: "normal",
				force: true,
			}),
		{ fallback: "layout analysis enqueue failed" },
	);
}
