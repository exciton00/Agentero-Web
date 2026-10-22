/**
 * Count PDF pages in an already-read buffer without full layout analysis.
 * Uses a throwaway @embedpdf document-manager registry on the caller's shared
 * headless engine, so neither the buffer nor a document handle outlives it.
 */

import { PluginRegistry } from "@embedpdf/core";
import type { PdfEngine } from "@embedpdf/models";
import type { DocumentManagerCapability } from "@embedpdf/plugin-document-manager";
import { DocumentManagerPluginPackage } from "@embedpdf/plugin-document-manager";
import { RenderPluginPackage } from "@embedpdf/plugin-render";
import { nanoid } from "nanoid";

/**
 * @returns Page count, or null when the document cannot be opened.
 */
export async function getPdfPageCountFromBuffer(
	engine: PdfEngine,
	buffer: ArrayBuffer,
): Promise<number | null> {
	const documentId = `page-count-${nanoid()}`;
	const registry = new PluginRegistry(engine);
	let docCap: DocumentManagerCapability | null = null;
	try {
		registry.registerPlugin(DocumentManagerPluginPackage, {});
		registry.registerPlugin(RenderPluginPackage);
		await registry.initialize();
		await registry.pluginsReady();
		const docPlugin = registry.getPlugin("document-manager");
		if (!docPlugin) return null;
		docCap = (
			docPlugin as unknown as { provides: () => DocumentManagerCapability }
		).provides();
		const openRes = await docCap
			.openDocumentBuffer({
				buffer,
				documentId,
				name: "page-count",
				autoActivate: true,
			})
			.toPromise();
		const doc = await openRes.task.toPromise();
		const pageCount = doc.pageCount ?? 0;
		return pageCount > 0 ? pageCount : null;
	} catch {
		return null;
	} finally {
		// Release this probe's document, never the shared engine.
		try {
			if (docCap) await docCap.closeDocument(documentId).toPromise();
		} catch {
			// Registry teardown retries documents still in its store.
		}
		try {
			await registry.destroy();
		} catch {
			// Cleanup must not replace a page count or the null failure result.
		}
	}
}
