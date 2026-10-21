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
import { notifyError, notifySuccess } from "@/lib/core/notify";
import { paperTagsChange } from "@/lib/paper/library-actions";
import { editablePaperTags, type PaperTag } from "@/lib/paper/tags";
import { refreshTagsIndex } from "@/lib/paper/tags-index";
import type { PaperMetadata } from "@/lib/paper/types";
import { getVaultPath } from "@/lib/vault/store";

export function AddPaperTagsDialog({
	paper,
	papers,
	open,
	suggestions,
	onOpenChange,
}: {
	/** Single target (row context menu). */
	paper?: PaperMetadata | null;
	/** Batch targets: the dialog tags are merged into every paper. */
	papers?: PaperMetadata[];
	open: boolean;
	suggestions: PaperTag[];
	onOpenChange: (open: boolean) => void;
}) {
	const { t } = useTranslation(["sidebar", "common"]);
	const [draft, setDraft] = useState("");
	const [tags, setTags] = useState<PaperTag[]>([]);
	const [busy, setBusy] = useState(false);

	/** Deduped targets: batch list wins, single row kept for compatibility. */
	const targets = useMemo(() => {
		const list = papers && papers.length > 0 ? papers : paper ? [paper] : [];
		const byKey = new Map<string, PaperMetadata>();
		for (const target of list) {
			const key = target.path?.trim() || target.id;
			if (key) byKey.set(key, target);
		}
		return [...byKey.values()];
	}, [paper, papers]);
	const batch = targets.length > 1;

	// Re-seed the draft whenever the dialog opens for (possibly other) papers.
	useEffect(() => {
		if (!open || targets.length === 0) return;
		setTags(
			batch ? [] : (editablePaperTags(targets[0].tags ?? []) as PaperTag[]),
		);
		setDraft("");
	}, [open, targets, batch]);

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
		if (targets.length === 0 || busy) return;
		const pending = draft.trim();
		const next =
			pending && !selected.has(pending) ? [...tags, { name: pending }] : tags;
		setBusy(true);
		try {
			let saved = 0;
			let failed = 0;
			for (const target of targets) {
				const current = editablePaperTags(target.tags ?? []) as PaperTag[];
				const existing = new Set(current.map((tag) => tag.name));
				// Batch: keep each paper's own tags and merge the dialog tags in.
				// Single: the dialog is the full tag set (supports removal).
				const merged = batch
					? [...current, ...next.filter((tag) => !existing.has(tag.name))]
					: next;
				try {
					const updated = await paperTagsChange(target, merged);
					if (updated) saved += 1;
					else failed += 1;
				} catch {
					failed += 1;
				}
			}
			if (saved > 0) {
				notifySuccess(
					batch
						? t("papersLibrary.batchTagsSaved", { count: saved })
						: t("papersLibrary.addTagSaved"),
				);
				void refreshTagsIndex(getVaultPath());
				onOpenChange(false);
			}
			if (failed > 0) {
				notifyError(t("paperInfo.tagsSaveFailed"), {
					description: t("papersLibrary.batchTagsPartial", { failed }),
				});
			}
		} finally {
			setBusy(false);
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-md">
				<DialogHeader>
					<DialogTitle>
						{batch
							? t("papersLibrary.batchAddTagTitle", {
									count: targets.length,
								})
							: t("papersLibrary.addTagTitle")}
					</DialogTitle>
				</DialogHeader>
				{batch ? (
					<p className="text-muted-foreground text-xs">
						{t("papersLibrary.batchAddTagHint", { count: targets.length })}
					</p>
				) : null}
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
