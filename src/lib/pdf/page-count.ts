/**
 * Get PDF page count without full layout analysis.
 * Reads the PDF off the shared headless engine, then counts pages in a
 * throwaway document-manager registry (see buffer-page-count.ts).
 */

import { findLocalPdfPath, localFileToArrayBuffer } from "@/lib/paper";
import { getPdfPageCountFromBuffer } from "@/lib/pdf/buffer-page-count";
import { getHeadlessPdfEngine } from "@/lib/pdf/layout/headless-analyze";

/**
 * Get PDF page count for a paper folder.
 * Returns null if no PDF found or failed to read.
 */
export async function getPdfPageCount(
	paperAbsPath: string,
): Promise<number | null> {
	try {
		const pdfPath = await findLocalPdfPath(paperAbsPath);
		if (!pdfPath) return null;

		const buffer = await localFileToArrayBuffer(pdfPath);
		if (!buffer) return null;

		const engine = await getHeadlessPdfEngine();
		return await getPdfPageCountFromBuffer(engine, buffer);
	} catch {
		return null;
	}
}
