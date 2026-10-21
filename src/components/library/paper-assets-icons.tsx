/**
 * Row-end asset status badges for the Library table.
 *
 * - PDF: red Acrobat-style file icon when a PDF is on disk, grey otherwise.
 * - Layout analysis: self-drawn blue rounded "md" box when a layout result
 *   exists (sidecar or index), grey otherwise.
 *
 * Both icons are fixed-size and sit in a narrow trailing cell so they never
 * squeeze the configurable data columns.
 */
import { FaFilePdf } from "react-icons/fa6";
import { cn } from "@/lib/core/utils";

const ICON_CLASS = "size-3.5 shrink-0";

export function PdfAssetIcon({
	active,
	label,
}: {
	active: boolean;
	label: string;
}) {
	return (
		<FaFilePdf
			role="img"
			aria-label={label}
			title={label}
			className={cn(
				ICON_CLASS,
				active ? "text-red-500" : "text-muted-foreground/40",
			)}
		/>
	);
}

/** Hand-drawn rounded square with a tiny "md" — marks a finished layout pass. */
export function LayoutAssetIcon({
	active,
	label,
}: {
	active: boolean;
	label: string;
}) {
	return (
		<svg
			viewBox="0 0 16 16"
			role="img"
			aria-label={label}
			className={cn(
				ICON_CLASS,
				active ? "text-blue-500" : "text-muted-foreground/40",
			)}
		>
			<title>{label}</title>
			<rect
				x="1.6"
				y="1.6"
				width="12.8"
				height="12.8"
				rx="3.2"
				fill={active ? "currentColor" : "none"}
				fillOpacity={active ? 0.14 : 0}
				stroke="currentColor"
				strokeWidth="1.4"
			/>
			<text
				x="8"
				y="10.9"
				textAnchor="middle"
				fontSize="6.1"
				fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace"
				fontWeight="700"
				fill="currentColor"
				stroke="none"
			>
				md
			</text>
		</svg>
	);
}

/** Fixed-width trailing badges shared by every Library row. */
export function PaperAssetsBadges({
	hasPdf,
	hasLayout,
	pdfLabel,
	layoutLabel,
}: {
	hasPdf: boolean;
	hasLayout: boolean;
	pdfLabel: string;
	layoutLabel: string;
}) {
	return (
		<span className="inline-flex items-center justify-end gap-1.5">
			<PdfAssetIcon active={hasPdf} label={pdfLabel} />
			<LayoutAssetIcon active={hasLayout} label={layoutLabel} />
		</span>
	);
}
