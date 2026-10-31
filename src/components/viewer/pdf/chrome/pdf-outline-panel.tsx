import type { PdfBookmarkObject } from "@embedpdf/models";
import { memo } from "react";
import { useTranslation } from "react-i18next";
import { OutlineTree } from "@/components/viewer/pdf/chrome/outline-tree";
import { PDF_SIDE_PANEL } from "@/components/viewer/pdf/chrome/pdf-chrome-surface";
import type { PdfAnchorNavItem } from "@/components/viewer/pdf/hooks/use-pdf-document-anchors";
import { cn } from "@/lib/core/utils";

type PdfOutlinePanelProps = {
	/** Document bookmarks; both the toggle and the panel hide when empty. */
	outline: PdfBookmarkObject[];
	/** Numbered section / paragraph anchors from the layout sidecar. */
	anchors: PdfAnchorNavItem[];
	showOutline: boolean;
	onGoToPage: (page: number) => void;
	onGoToAnchor: (anchor: PdfAnchorNavItem) => void;
};

/** Collapsible bookmark + section sidebar. The toggle lives in PdfLeftToolbar. */
export const PdfOutlinePanel = memo(function PdfOutlinePanel({
	outline,
	anchors,
	showOutline,
	onGoToPage,
	onGoToAnchor,
}: PdfOutlinePanelProps) {
	const { t } = useTranslation("viewer");
	if (!showOutline || (outline.length === 0 && anchors.length === 0)) {
		return null;
	}

	return (
		<aside data-pdf-chrome className={cn("agentero-scroll", PDF_SIDE_PANEL)}>
			<div className="px-2">
				{outline.length ? (
					<OutlineTree nodes={outline} depth={0} onGoToPage={onGoToPage} />
				) : null}
				{anchors.length ? (
					<div
						className={cn(
							"space-y-0.5",
							outline.length && "mt-2 border-border/50 border-t pt-2",
						)}
					>
						<div className="px-2 py-1 font-medium text-caption text-muted-foreground uppercase tracking-wide">
							{t("pdf.sections")}
						</div>
						<ul className="space-y-0.5">
							{anchors.map((item) => (
								<li key={item.regionId}>
									<button
										type="button"
										className="flex w-full items-center gap-1 rounded px-2 py-1 text-left text-muted-foreground text-sm transition-colors duration-100 hover:bg-muted/60 hover:text-foreground active:bg-muted/80"
										style={{ paddingLeft: 8 + item.depth * 12 }}
										title={item.label}
										onClick={() => onGoToAnchor(item)}
									>
										<span className="min-w-0 flex-1 truncate">
											{item.label}
										</span>
										{item.anchor ? (
											<span className="shrink-0 text-caption text-muted-foreground/70 tabular-nums">
												{item.anchor}
											</span>
										) : null}
									</button>
								</li>
							))}
						</ul>
					</div>
				) : null}
			</div>
		</aside>
	);
});
