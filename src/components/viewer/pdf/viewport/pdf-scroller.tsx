import { Scroller, useScroll } from "@embedpdf/plugin-scroll/react";
import { type ComponentProps, useCallback } from "react";
import { useSettings } from "@/hooks/use-app-stores";

/** Keep EmbedPDF's document coordinates for links, selections and scroll sync.
 * Only the current page paints in paginated mode; virtual spacers stay intact.
 */
export function PdfScroller(props: ComponentProps<typeof Scroller>) {
	const paginated = useSettings((s) => s.pdfScrollStrategy === "paginated");
	const { state } = useScroll(props.documentId);
	const { renderPage } = props;
	const renderVisiblePage = useCallback(
		(page: Parameters<typeof renderPage>[0]) =>
			!paginated || page.pageNumber === state.currentPage
				? renderPage(page)
				: null,
		[paginated, state.currentPage, renderPage],
	);
	return <Scroller {...props} renderPage={renderVisiblePage} />;
}
