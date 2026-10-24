/**
 * Public surface of the document viewers.
 *
 * Everything else under `viewer/` is internal: `pdf/` is the EmbedPDF reader
 * (shell + `hooks/` + `layers/` + `chrome/` + `cards/` + `viewport/`) and
 * `panels/` holds the right-rail panels. Import those paths only from within
 * `viewer/` — never import this barrel from inside the folder (import cycle).
 *
 * Exception: lazy `import()` call sites (`src/main.tsx`, `workspace/doc-view`,
 * the mobile reader) keep importing the concrete module, since resolving them
 * through this barrel would pull every viewer and panel into that chunk.
 */

export { HtmlViewer } from "@/components/viewer/html-viewer";
export { ImageViewer } from "@/components/viewer/image-viewer";
export {
	type AnnotationRow,
	AnnotationsPanel,
	type AskRow,
	type VisualTraceRow,
} from "@/components/viewer/panels/annotations-panel";
export {
	PdfViewer,
	type PdfViewerHandle,
} from "@/components/viewer/pdf/pdf-viewer";
export {
	pdfHandleFor,
	registerPdfHandle,
	resolveActivePdfHandle,
} from "@/lib/workspace/viewer/pdf-viewer-registry";
