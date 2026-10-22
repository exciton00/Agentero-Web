/**
 * Headless layout analysis: open a paper PDF off-viewer, run PP-DocLayoutV3,
 * write `{paper}/source/layout.json`. Used after download/import so sidecar is
 * ready before the user opens the paper.
 */

import { PluginRegistry } from "@embedpdf/core";
import type { PdfEngine } from "@embedpdf/models";
import { AiManagerPluginPackage } from "@embedpdf/plugin-ai-manager";
import type { DocumentManagerCapability } from "@embedpdf/plugin-document-manager";
import { DocumentManagerPluginPackage } from "@embedpdf/plugin-document-manager";
import {
	type LayoutAnalysisCapability,
	LayoutAnalysisPluginPackage,
} from "@embedpdf/plugin-layout-analysis";
import { RenderPluginPackage } from "@embedpdf/plugin-render";
import i18n from "@/i18n";
import { errorText } from "@/lib/core/error";
import { notifyWarning } from "@/lib/core/notify";

import { findCanonicalPaperPdfPath, localFileToArrayBuffer } from "@/lib/paper";
import { getPdfPageCountFromBuffer } from "@/lib/pdf/buffer-page-count";
import { createPdfEngine } from "@/lib/pdf/engine";
import { getPdfAiRuntime } from "@/lib/pdf/layout/ai-runtime";
import {
	layoutSidecarBlocksAnalysis,
	readLayoutSidecar,
	writeLayoutIndexFromRaw,
	writeLayoutSidecar,
} from "@/lib/pdf/layout/io";
import {
	invokeLayoutRemoteAnalyzePdf,
	paddlePageToRegions,
} from "@/lib/pdf/layout/paddle";
import {
	isRemoteLayoutProvider,
	layoutProviderFor,
} from "@/lib/pdf/layout/providers";
import type { LayoutTaskLike } from "@/lib/pdf/layout/run-analysis";
import { runDocumentLayoutAnalysis } from "@/lib/pdf/layout/run-analysis";
import { layoutMaxPages } from "@/lib/pdf/layout/settings";
import { clearLayoutDocumentResult } from "@/lib/pdf/layout/store";
import type { PdfLayoutRegion } from "@/lib/pdf/layout/types";
import { loadSettings } from "@/lib/settings/store";

let analysisEngine: PdfEngine | null = null;
let analysisEnginePromise: Promise<PdfEngine> | null = null;

export async function getHeadlessPdfEngine(): Promise<PdfEngine> {
	if (analysisEngine) return analysisEngine;
	if (analysisEnginePromise) return analysisEnginePromise;
	const pending = createPdfEngine().then((engine) => {
		analysisEngine = engine;
		return engine;
	});
	analysisEnginePromise = pending;
	try {
		return await pending;
	} finally {
		if (analysisEnginePromise === pending) analysisEnginePromise = null;
	}
}

export type HeadlessLayoutResult = {
	fromCache: boolean;
	summary: string;
	regionCount: number;
};

/**
 * Ensure layout sidecar exists for a paper folder. Hits cache when present
 * unless `force` is set. Opens `{paper}/{id}.pdf` only — never an
 * illustration PDF under `source/assets/`.
 */
export async function analyzePaperLayoutHeadless(opts: {
	paperAbsPath: string;
	/** Label shown in the background-task toast instead of "Analyzing layout…". */
	paperLabel?: string;
	/** Caller-owned EmbedPDF document id, so progress can be attributed to this run. */
	documentId?: string;
	/**
	 * Re-run even when `source/layout.json` already has regions. JobCenter
	 * stores this on the task; it has to be read here or a force enqueue
	 * still returns the cached sidecar.
	 */
	force?: boolean;
	signal?: AbortSignal;
}): Promise<HeadlessLayoutResult> {
	const paperAbsPath = opts.paperAbsPath.replace(/[/\\]+$/, "");
	if (opts.signal?.aborted) throw new Error("cancelled");

	const existing = await readLayoutSidecar(paperAbsPath);
	if (opts.signal?.aborted) throw new Error("cancelled");
	if (
		existing &&
		layoutSidecarBlocksAnalysis(opts.force, existing.regions.length)
	) {
		// Ensure CLI-facing sidebar index exists even on raw-only cache hits.
		try {
			if (opts.signal?.aborted) throw new Error("cancelled");
			await writeLayoutIndexFromRaw(paperAbsPath, existing.regions);
		} catch (error) {
			if (opts.signal?.aborted || errorText(error) === "cancelled") {
				throw new Error("cancelled");
			}
			// non-fatal
		}
		return {
			fromCache: true,
			summary: `cached ${existing.regions.length} regions`,
			regionCount: existing.regions.length,
		};
	}

	const pdfPath = await findCanonicalPaperPdfPath(paperAbsPath);
	if (!pdfPath) {
		// Soft-skip: callers may race ahead of DownloadAssets. Re-enqueue after
		// the PDF lands rather than failing the job with "No local PDF".
		return {
			fromCache: false,
			summary: "skipped: no local PDF",
			regionCount: 0,
		};
	}
	const buffer = await localFileToArrayBuffer(pdfPath);
	if (!buffer) {
		throw new Error("Failed to read paper PDF");
	}
	if (opts.signal?.aborted) throw new Error("cancelled");

	const maxPages = layoutMaxPages(loadSettings().layout);
	/**
	 * Soft skip: a PDF above the configured page cap succeeds without regions
	 * (no error) and tells the user why. The Host enforces the same cap.
	 */
	const skipOverPageLimit = (pages: number): HeadlessLayoutResult => {
		notifyWarning(
			i18n.t("settings:layout.maxPages.skipped", {
				pages,
				limit: maxPages,
			}),
			{ id: "layout-page-limit-skipped" },
		);
		return {
			fromCache: false,
			summary: `skipped: ${pages} pages > limit ${maxPages}`,
			regionCount: 0,
		};
	};

	// Remote layout backend (Settings → Layout): the Host owns the API key and
	// talks to MinerU / Paddle, so the browser does not need the ONNX model.
	// Falls through to local ONNX when no remote provider is configured.
	const layoutProvider = layoutProviderFor(loadSettings().layout.backend);
	if (layoutProvider && isRemoteLayoutProvider(layoutProvider)) {
		// The remote run uploads the whole PDF: count pages first so an
		// oversized paper is skipped locally instead of being transferred.
		if (maxPages > 0) {
			const engine = await getHeadlessPdfEngine();
			const bufferPages = await getPdfPageCountFromBuffer(engine, buffer);
			if (opts.signal?.aborted) throw new Error("cancelled");
			if (bufferPages != null && bufferPages > maxPages) {
				return skipOverPageLimit(bufferPages);
			}
		}
		const requestId = `headless-layout-${Date.now().toString(36)}`;
		const result = await invokeLayoutRemoteAnalyzePdf({
			provider: layoutProvider.id,
			pdfBase64: arrayBufferToBase64(buffer),
			fileName: pdfPath.split(/[/\\]/).pop() || "paper.pdf",
			requestId,
		});
		if (opts.signal?.aborted) throw new Error("cancelled");
		const raw: PdfLayoutRegion[] = [];
		result.pages.forEach((page, pageIndex) => {
			// MinerU reports PDF points (scale 1); Paddle reports rendered px.
			const width = page.widthPx && page.widthPx > 0 ? page.widthPx : 1224;
			const height = page.heightPx && page.heightPx > 0 ? page.heightPx : 1584;
			raw.push(
				...paddlePageToRegions({
					page,
					pageIndex,
					pageWidth: width,
					pageHeight: height,
					idPrefix: layoutProvider.id,
				}),
			);
		});
		await writeLayoutSidecar(paperAbsPath, raw, layoutProvider.sidecarMode);
		await writeLayoutIndexFromRaw(paperAbsPath, raw);
		return {
			fromCache: false,
			summary: `${layoutProvider.id}: ${raw.length} regions`,
			regionCount: raw.length,
		};
	}

	const engine = await getHeadlessPdfEngine();
	const documentId =
		opts.documentId ?? `headless-layout-${Date.now().toString(36)}`;
	const registry = new PluginRegistry(engine);
	registry.registerPlugin(DocumentManagerPluginPackage, {});
	registry.registerPlugin(RenderPluginPackage);
	registry.registerPlugin(AiManagerPluginPackage, {
		runtime: getPdfAiRuntime(),
	});
	registry.registerPlugin(LayoutAnalysisPluginPackage, {
		layoutThreshold: 0.3,
		tableStructure: false,
		autoAnalyze: false,
		renderScale: 2,
	});

	try {
		await registry.initialize();
		await registry.pluginsReady();
		if (opts.signal?.aborted) throw new Error("cancelled");

		const docPlugin = registry.getPlugin("document-manager");
		const layoutPlugin = registry.getPlugin("layout-analysis");
		if (!docPlugin || !layoutPlugin) {
			throw new Error("Layout plugins failed to register");
		}
		const docCap = (
			docPlugin as unknown as { provides: () => DocumentManagerCapability }
		).provides();
		const layoutCap = (
			layoutPlugin as unknown as { provides: () => LayoutAnalysisCapability }
		).provides();

		const openRes = await docCap
			.openDocumentBuffer({
				buffer,
				documentId,
				name: "layout-analysis",
				autoActivate: true,
			})
			.toPromise();
		const doc = await openRes.task.toPromise();
		const pageCount = doc.pageCount ?? 0;
		if (opts.signal?.aborted) throw new Error("cancelled");
		if (maxPages > 0 && pageCount > maxPages) {
			return skipOverPageLimit(pageCount);
		}

		const scope = layoutCap.forDocument(documentId);
		const summary = await new Promise<string>((resolve, reject) => {
			let settled = false;
			let layoutTask: LayoutTaskLike | null = null;
			const finish = (fn: () => void) => {
				if (settled) return;
				settled = true;
				opts.signal?.removeEventListener("abort", onAbort);
				fn();
			};
			function onAbort() {
				layoutTask?.abort({ type: "no-document", message: "cancelled" });
				layoutTask = null;
				finish(() => reject(new Error("cancelled")));
			}
			opts.signal?.addEventListener("abort", onAbort);
			void runDocumentLayoutAnalysis(scope, documentId, {
				paperAbsPath,
				paperLabel: opts.paperLabel,
				totalPages: pageCount > 0 ? pageCount : null,
				force: opts.force === true,
				pageSizeAt: (pageIndex) => {
					const size = doc.pages[pageIndex]?.size;
					return size && size.width > 0 && size.height > 0 ? size : null;
				},
				isDocumentOpen: () => docCap.isDocumentOpen(documentId),
				onDone: (s) => finish(() => resolve(s)),
				onError: (message, aborted) => {
					finish(() => {
						if (aborted) reject(new Error("cancelled"));
						else reject(new Error(message));
					});
				},
			})
				.then((task) => {
					layoutTask = task;
					// Cache path: onDone already fired before null return.
					if (task == null && !settled) {
						finish(() => resolve("cached"));
					} else if (task && opts.signal?.aborted) {
						onAbort();
					}
				})
				.catch((e) =>
					finish(() => reject(e instanceof Error ? e : new Error(String(e)))),
				);
		});

		const sidecar = await readLayoutSidecar(paperAbsPath);
		return {
			fromCache: false,
			summary,
			regionCount: sidecar?.regions?.length ?? 0,
		};
	} finally {
		try {
			const docPlugin = registry.getPlugin("document-manager");
			if (docPlugin) {
				const docCap = (
					docPlugin as unknown as {
						provides: () => DocumentManagerCapability;
					}
				).provides();
				await docCap
					.closeDocument(documentId)
					.toPromise()
					.catch(() => undefined);
			}
		} catch {
			// ignore close errors
		}
		try {
			await registry.destroy();
		} catch {
			// ignore
		}
		clearLayoutDocumentResult(documentId);
	}
}

/** Base64-encode PDF bytes for the Host-side remote layout request. */
function arrayBufferToBase64(buffer: ArrayBuffer): string {
	const bytes = new Uint8Array(buffer);
	let binary = "";
	const chunk = 0x8000;
	for (let i = 0; i < bytes.length; i += chunk) {
		binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
	}
	return btoa(binary);
}
