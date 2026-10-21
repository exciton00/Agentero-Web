import { commands } from "@/lib/core/bindings";
import { callApi } from "@/lib/core/ipc";
import { hasHostTransport } from "@/lib/core/tauri";
import {
	aggregateReadingHeatmap,
	emptyHeatmap,
} from "@/lib/paper/reading-heatmap/aggregate";
import type {
	ReadingActivityPoint,
	ReadingHeatmap,
} from "@/lib/paper/reading-heatmap/types";

export type ReadingHeatmapBatch = {
	heatmaps: Map<string, ReadingHeatmap>;
	/**
	 * Raw activity points per key — cached by the caller so a later lazy
	 * page-count discovery can re-aggregate without re-reading `marks/`.
	 */
	points: Map<string, ReadingActivityPoint[]>;
};

/**
 * One-IPC batch read of `marks/*.json` reading activity for many papers
 * (Host `paper_reading_activity_batch`). Replaces the per-paper
 * highlights/asks/translates fan-out (3 IPC × N papers).
 * Non-Tauri and remote vaults resolve to empty activity (same as the old
 * per-paper reads, which also returned nothing there).
 */
async function fetchReadingActivityBatch(
	vaultPath: string,
	rels: string[],
): Promise<Record<string, ReadingActivityPoint[]>> {
	if (!hasHostTransport() || !rels.length) return {};
	const { isRemoteVaultHandle } = await import(
		"@/lib/vault/remote/remote-vault"
	);
	if (isRemoteVaultHandle(vaultPath)) return {};
	try {
		// Wire points type `y`/`weight` as nullable; JS arithmetic in the
		// aggregator treats `null` as 0 (unchanged from the pre-binding cast).
		return (await callApi(
			() => commands.paperReadingActivityBatch({ vaultPath, paths: rels }),
			{ fallback: "paper_reading_activity_batch failed" },
		)) as Record<string, ReadingActivityPoint[]>;
	} catch {
		return {};
	}
}

/**
 * Load heatmaps for many papers with a single batch IPC.
 * Keys are vault-relative paper paths (or id when path missing).
 * `opts.pageCounts` (same keys) normalizes bins to the real document extent;
 * papers without a cached count fall back to the max observed page — this
 * function never opens a PDF (page counts are discovered lazily elsewhere).
 */
export async function loadReadingHeatmaps(
	vaultPath: string,
	papers: ReadonlyArray<{ path?: string; id: string }>,
	opts?: { pageCounts?: ReadonlyMap<string, number> },
): Promise<ReadingHeatmapBatch> {
	const heatmaps = new Map<string, ReadingHeatmap>();
	const points = new Map<string, ReadingActivityPoint[]>();
	if (!vaultPath || !papers.length) return { heatmaps, points };

	const keyed: Array<{ key: string; rel: string | null }> = papers.map(
		(paper) => {
			const rel =
				paper.path?.replace(/\\/g, "/").replace(/^\/+|\/+$/g, "") || null;
			return { key: rel || paper.id, rel };
		},
	);
	const rels = [...new Set(keyed.flatMap((k) => (k.rel ? [k.rel] : [])))];
	const activity = await fetchReadingActivityBatch(vaultPath, rels);

	for (const { key, rel } of keyed) {
		const pts = (rel ? activity[rel] : undefined) ?? [];
		points.set(key, pts);
		const pageCount = opts?.pageCounts?.get(key);
		heatmaps.set(
			key,
			!pts.length && !pageCount
				? emptyHeatmap()
				: aggregateReadingHeatmap(pts, { pageCount }),
		);
	}
	return { heatmaps, points };
}

export function heatmapCacheKey(paper: { path?: string; id: string }): string {
	const rel = paper.path?.replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
	return rel || paper.id;
}
