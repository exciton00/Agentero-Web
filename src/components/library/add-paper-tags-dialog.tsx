/**
 * Small tag editor opened from the Library row context menu ("添加标签").
 *
 * Reuses the existing tag RPC path (`paperTagsChange` → `paper_tags_change`),
 * the shared `PaperTagChip` rendering, and the Library's tag suggestions.
 * Saving updates the catalog, the library row, open tabs, and refreshes the
 * tag index behind the "expand by tag" tree node.
 */
import { X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
	PaperTagChip,
	PaperTagRemoveButton,
} from "@/components/library/paper-tag-chip";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { errorMessage, notifyError, notifySuccess } from "@/lib/core/notify";
import { paperTagsChange } from "@/lib/paper/library-actions";
import { editablePaperTags, type PaperTag } from "@/lib/paper/tags";
import { refreshTagsIndex } from "@/lib/paper/tags-index";
import type { PaperMetadata } from "@/lib/paper/types";
import { getVaultPath } from "@/lib/vault/store";

export function AddPaperTagsDialog({
	paper,
	open,
	suggestions,
	onOpenChange,
}: {
	paper: PaperMetadata | null;
	open: boolean;
	suggestions: PaperTag[];
	onOpenChange: (open: boolean) => void;
}) {
	const { t } = useTranslation(["sidebar", "common"]);
	const [draft, setDraft] = useState("");
	const [tags, setTags] = useState<PaperTag[]>([]);
	const [busy, setBusy] = useState(false);

	// Re-seed the draft whenever the dialog opens for a (possibly other) paper.
	useEffect(() => {
		if (!open || !paper) return;
		setTags(editablePaperTags(paper.tags ?? []) as PaperTag[]);
		setDraft("");
	}, [open, paper]);

	const selected = useMemo(() => new Set(tags.map((tag) => tag.name)), [tags]);
	const unusedSuggestions = useMemo(
		() => suggestions.filter((tag) => !selected.has(tag.name)).slice(0, 12),
		[suggestions, selected],
	);

	const addTag = (name: string) => {
		const trimmed = name.trim();
		if (!trimmed || selected.has(trimmed)) return;
		setTags((prev) => [...prev, { name: trimmed }]);
		setDraft("");
	};

	const removeTag = (name: string) => {
		setTags((prev) => prev.filter((tag) => tag.name !== name));
	};

	const save = async () => {
		if (!paper || busy) return;
		const pending = draft.trim();
		const next =
			pending && !selected.has(pending) ? [...tags, { name: pending }] : tags;
		setBusy(true);
		try {
			const updated = await paperTagsChange(paper, next);
			if (!updated) return;
			notifySuccess(t("papersLibrary.addTagSaved"));
			void refreshTagsIndex(getVaultPath());
			onOpenChange(false);
		} catch (error) {
			notifyError(t("paperInfo.tagsSaveFailed"), {
				description: errorMessage(error),
			});
		} finally {
			setBusy(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-md">
				<DialogHeader>
					<DialogTitle>{t("papersLibrary.addTagTitle")}</DialogTitle>
				</DialogHeader>
				<div className="space-y-3">
					<div className="flex flex-wrap gap-1.5">
						{tags.length === 0 ? (
							<span className="text-muted-foreground text-xs">
								{t("papersLibrary.noTagsYet")}
							</span>
						) : (
							tags.map((tag) => (
								<PaperTagChip
									key={tag.name}
									tag={tag}
									trailing={
										<PaperTagRemoveButton
											tagName={tag.name}
											label={t("paperInfo.removeTag", { tag: tag.name })}
											disabled={busy}
											onRemove={removeTag}
										/>
									}
								/>
							))
						)}
					</div>
					<Input
						autoFocus
						value={draft}
						placeholder={t("papersLibrary.addTagPlaceholder")}
						spellCheck={false}
						autoComplete="off"
						onChange={(event) => setDraft(event.currentTarget.value)}
						onKeyDown={(event) => {
							if (event.key === "Enter") {
								event.preventDefault();
								addTag(draft);
							}
						}}
					/>
					{unusedSuggestions.length > 0 ? (
						<div className="space-y-1.5">
							<p className="text-muted-foreground text-xs">
								{t("papersLibrary.tagSuggestions")}
							</p>
							<div className="flex flex-wrap gap-1.5">
								{unusedSuggestions.map((tag) => (
									<PaperTagChip
										key={tag.name}
										tag={tag}
										aria-label={tag.name}
										onClick={() => addTag(tag.name)}
									/>
								))}
							</div>
						</div>
					) : null}
				</div>
				<DialogFooter>
					<Button
						type="button"
						variant="ghost"
						disabled={busy}
						onClick={() => onOpenChange(false)}
					>
						<X className="size-4" aria-hidden />
						{t("common:cancel")}
					</Button>
					<Button type="button" disabled={busy} onClick={() => void save()}>
						{t("common:save")}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
