/**
 * Second confirmation for the Library's batch delete (moves every selected
 * paper folder to the recycle bin). Single-row delete keeps the existing
 * no-confirm flow because it is immediately undoable from the bin.
 */
import { Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";

export function ConfirmBatchDeleteDialog({
	open,
	count,
	busy,
	onOpenChange,
	onConfirm,
}: {
	open: boolean;
	count: number;
	busy: boolean;
	onOpenChange: (open: boolean) => void;
	onConfirm: () => void;
}) {
	const { t } = useTranslation(["sidebar", "common"]);
	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-md">
				<DialogHeader>
					<DialogTitle>
						{t("papersLibrary.batchDeleteConfirmTitle", { count })}
					</DialogTitle>
				</DialogHeader>
				<p className="text-muted-foreground text-sm leading-relaxed">
					{t("papersLibrary.batchDeleteConfirmBody", { count })}
				</p>
				<DialogFooter>
					<Button
						type="button"
						variant="ghost"
						disabled={busy}
						onClick={() => onOpenChange(false)}
					>
						{t("common:cancel")}
					</Button>
					<Button
						type="button"
						variant="destructive"
						disabled={busy}
						onClick={onConfirm}
					>
						<Trash2 className="size-4" aria-hidden />
						{t("papersLibrary.batchDeleteConfirmAction")}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
