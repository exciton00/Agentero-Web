/**
 * Section / paragraph anchors for the PDF side panel.
 *
 * Anchors are enumerated from `source/layout.json` (the same raw sidecar the
 * layout analysis writes) using the shared {@link documentAnchors} model, so
 * the panel numbering matches what completion / citations expect (`#2.1`,
 * `#2.1#p1`). Reading order repairs two-column layouts before numbering.
 *
 * The sidecar is read lazily per paper and cached; a layout write invalidates
 * the cache so a re-analysis updates the panel without reopening the PDF.
 */

import { useEffect, useState } from "react";
import type { PdfAskNormalizedRect } from "@/lib/pdf/ask/types";
import {
	type DocumentAnchor,
	documentAnchors,
} from "@/lib/pdf/layout/document-anchors";
import {
	readLayoutSidecar,
	subscribeLayoutSidecarWrite,
} from "@/lib/pdf/layout/io";
import type {
	DocumentAnchorRegion,
	PdfLayoutKind,
	PdfLayoutRegion,
} from "@/lib/pdf/layout/types";

export type PdfAnchorNavItem = {
	/** Raw layout region id; the jump target. */
	regionId: string;
	kind: "section" | "paragraph";
	/** `#2.1` or `#2.1#p1`; null when the owning section number is ambiguous. */
	anchor: string | null;
	/** Display label: section title / paragraph text. */
	label: string;
	/** Nesting level: 0 = top-level section, 1+ = subsection / paragraph. */
	depth: number;
	pageIndex: number;
	/** Normalized 0–1 box, used by the region jump. */
	bbox: PdfAskNormalizedRect;
	regionKind: PdfLayoutKind;
};

const anchorsByPaper = new Map<string, PdfAnchorNavItem[]>();

function paperCacheKey(paperAbsPath: string): string {
	return paperAbsPath.replaceAll("\\", "/").replace(/\/+$/, "");
}

subscribeLayoutSidecarWrite((paperAbsPath) => {
	anchorsByPaper.delete(paperCacheKey(paperAbsPath));
});

function stripHash(anchor: string): string {
	return anchor.startsWith("#") ? anchor.slice(1) : anchor;
}

function anchorDepth(anchor: string | null): number {
	if (!anchor) return 0;
	return Math.max(0, stripHash(anchor).split(".").length - 1);
}

function sectionLabel(anchor: DocumentAnchor): string {
	const title = anchor.title?.trim();
	const text = anchor.text?.trim();
	return title || text || (anchor.anchor ? stripHash(anchor.anchor) : "");
}

function paragraphLabel(anchor: DocumentAnchor): string {
	return anchor.text?.trim() || (anchor.anchor ? stripHash(anchor.anchor) : "");
}

function toRegion(region: PdfLayoutRegion): DocumentAnchorRegion {
	return {
		id: region.id,
		pageIndex: region.pageIndex,
		kind: region.kind,
		title: region.title,
		text: region.text,
		readingOrder: region.readingOrder,
		bbox: region.bbox,
	};
}

function regionKindOf(region: PdfLayoutRegion): PdfLayoutKind {
	return region.kind === "header" ? "header" : "text";
}

async function loadAnchorNavItems(
	paperAbsPath: string,
): Promise<PdfAnchorNavItem[]> {
	const key = paperCacheKey(paperAbsPath);
	const cached = anchorsByPaper.get(key);
	if (cached) return cached;
	const sidecar = await readLayoutSidecar(paperAbsPath);
	if (!sidecar) {
		anchorsByPaper.set(key, []);
		return [];
	}
	const regionById = new Map(sidecar.regions.map((r) => [r.id, r]));
	const anchors = documentAnchors(sidecar.regions.map(toRegion));
	const sectionDepths = new Map<string, number>();
	const items: PdfAnchorNavItem[] = [];
	for (const anchor of anchors) {
		const region = regionById.get(anchor.id);
		if (!region) continue;
		let depth = 0;
		if (anchor.kind === "section") {
			depth = anchorDepth(anchor.anchor);
			if (anchor.anchor) sectionDepths.set(anchor.anchor, depth);
		} else {
			depth =
				(anchor.sectionAnchor
					? (sectionDepths.get(anchor.sectionAnchor) ?? 0)
					: 0) + 1;
		}
		items.push({
			regionId: region.id,
			kind: anchor.kind,
			anchor: anchor.anchor,
			label:
				anchor.kind === "section"
					? sectionLabel(anchor)
					: paragraphLabel(anchor),
			depth,
			pageIndex: region.pageIndex,
			bbox: region.bbox,
			regionKind: regionKindOf(region),
		});
	}
	anchorsByPaper.set(key, items);
	return items;
}

/**
 * Anchors for one paper. `paperAbsPath` null (remote / plain viewer) returns
 * an empty list without touching the vault.
 */
export function usePdfDocumentAnchors(
	paperAbsPath: string | null,
): PdfAnchorNavItem[] {
	const [items, setItems] = useState<PdfAnchorNavItem[]>([]);

	useEffect(() => {
		if (!paperAbsPath) {
			setItems([]);
			return;
		}
		let cancelled = false;
		const reload = () => {
			void loadAnchorNavItems(paperAbsPath).then((next) => {
				if (!cancelled) setItems(next);
			});
		};
		reload();
		const target = paperCacheKey(paperAbsPath);
		const unsubscribe = subscribeLayoutSidecarWrite((changed) => {
			if (paperCacheKey(changed) !== target) return;
			reload();
		});
		return () => {
			cancelled = true;
			unsubscribe();
		};
	}, [paperAbsPath]);

	return items;
}
