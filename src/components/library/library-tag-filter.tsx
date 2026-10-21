/**
 * Library tag filter bar (one row, above the table).
 *
 * Left: the active AND-filter chips (click a chip to drop it) + clear action;
 * right: a searchable multi-select menu with color dots and paper counts.
 * The chip strip scrolls horizontally so the bar never grows to two lines.
 */
import { Check, ChevronDown, ListFilter, Search, X } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { PaperTagChip } from "@/components/library/paper-tag-chip";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/components/ui/popover";
import { useUiStore } from "@/hooks/use-app-stores";
import { cn } from "@/lib/core/utils";
import type { PaperTag } from "@/lib/paper/tags";
import type { TagsIndexTag } from "@/lib/paper/tags-index";
import { clearTagFilter, toggleTagFilter } from "@/lib/shell/ui-store";
import { isTagColorId, tagSwatchStyle } from "@/lib/ui/tag-colors";

export type LibraryTagFilterProps = {
	/** Selectable tags (vault tag index, `#`-reserved tags already removed). */
	tags: TagsIndexTag[];
	/** Vault-relative folder scope currently shown (null = whole library). */
	scopePath?: string | null;
	className?: string;
};

/** Index tag → chip tag (chip only renders known palette ids). */
function toPaperTag(tag: TagsIndexTag): PaperTag {
	return {
		name: tag.name,
		color: isTagColorId(tag.color) ? tag.color : undefined,
	};
}

export function LibraryTagFilter({
	tags,
	scopePath = null,
	className,
}: LibraryTagFilterProps) {
	const { t } = useTranslation("sidebar");
	const tagFilter = useUiStore((state) => state.tagFilter);
	const [open, setOpen] = useState(false);
	const [search, setSearch] = useState("");

	const tagByName = useMemo(
		() => new Map(tags.map((tag) => [tag.name, tag])),
		[tags],
	);
	const filteredTags = useMemo(() => {
		const query = search.trim().toLocaleLowerCase();
		if (!query) return tags;
		return tags.filter((tag) => tag.name.toLocaleLowerCase().includes(query));
	}, [tags, search]);

	return (
		<div
			className={cn(
				"sticky left-0 z-10 flex w-full min-w-0 items-center gap-2 border-border/40 border-b bg-background/85 px-3 py-1.5 whitespace-nowrap backdrop-blur",
				className,
			)}
		>
			<div className="flex min-w-[9rem] flex-1 items-center gap-1">
				<div className="flex min-w-0 items-center gap-1 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
					{tagFilter.length === 0 ? (
						<span className="truncate text-muted-foreground text-xs">
							{t("papersLibrary.allPapers")}
						</span>
					) : (
						tagFilter.map((name) => {
							const tag = tagByName.get(name);
							return (
								<PaperTagChip
									key={name}
									tag={tag ? toPaperTag(tag) : { name }}
									title={t("papersLibrary.removeTagFilter", { tag: name })}
									trailing={<X className="size-2.5" aria-hidden />}
									onClick={() => toggleTagFilter(name)}
								/>
							);
						})
					)}
				</div>
				{/* Outside the scroller so "clear" stays reachable when chips overflow. */}
				{tagFilter.length > 0 ? (
					<Button
						type="button"
						variant="ghost"
						size="xs"
						className="h-6 shrink-0 px-1.5 text-xs"
						onClick={clearTagFilter}
					>
						{t("papersLibrary.clearTagFilter")}
					</Button>
				) : null}
			</div>
			{scopePath ? (
				<span
					className="min-w-0 max-w-[16rem] shrink truncate text-muted-foreground text-xs"
					title={scopePath}
				>
					{t("papersLibrary.currentFolder", { path: scopePath })}
				</span>
			) : null}
			<Popover
				open={open}
				onOpenChange={(next) => {
					setOpen(next);
					if (!next) setSearch("");
				}}
			>
				<PopoverTrigger asChild>
					<Button
						type="button"
						variant="outline"
						size="xs"
						className="shrink-0"
						title={t("papersLibrary.tagFilter")}
					>
						<ListFilter className="size-3.5" aria-hidden />
						{t("papersLibrary.tagFilter")}
						<ChevronDown className="size-3 opacity-60" aria-hidden />
					</Button>
				</PopoverTrigger>
				<PopoverContent
					align="end"
					sideOffset={6}
					className="w-64 gap-1.5 p-2"
					onOpenAutoFocus={(event) => event.preventDefault()}
				>
					<div className="relative">
						<Search
							className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground"
							aria-hidden
						/>
						<Input
							// Focus the search box so typing filters immediately.
							autoFocus
							value={search}
							onChange={(event) => setSearch(event.target.value)}
							placeholder={t("papersLibrary.tagFilterSearch")}
							aria-label={t("papersLibrary.tagFilterSearch")}
							className="h-7 pl-7 text-xs"
						/>
					</div>
					{filteredTags.length === 0 ? (
						<p className="px-1 py-2 text-muted-foreground text-xs">
							{search.trim()
								? t("papersLibrary.tagFilterNoMatch")
								: t("papersLibrary.filterTagsEmpty")}
						</p>
					) : (
						<div className="flex max-h-64 flex-col gap-0.5 overflow-y-auto">
							{filteredTags.map((tag) => {
								const selected = tagFilter.includes(tag.name);
								return (
									<button
										key={tag.name}
										type="button"
										title={tag.name}
										aria-pressed={selected}
										className={cn(
											"flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-xs",
											"hover:bg-muted/60",
											selected && "bg-muted/50",
										)}
										onClick={() => toggleTagFilter(tag.name)}
									>
										<span
											className={cn(
												"flex size-3.5 shrink-0 items-center justify-center rounded-sm border",
												selected
													? "border-primary bg-primary text-primary-foreground"
													: "border-muted-foreground/40",
											)}
											aria-hidden
										>
											{selected ? <Check className="size-2.5" /> : null}
										</span>
										<span
											className={cn(
												"size-2 shrink-0 rounded-full",
												!isTagColorId(tag.color) && "bg-muted-foreground/50",
											)}
											style={
												isTagColorId(tag.color)
													? tagSwatchStyle(tag.color)
													: undefined
											}
											aria-hidden
										/>
										<span className="min-w-0 flex-1 truncate">{tag.name}</span>
										{tag.count > 0 ? (
											<span className="shrink-0 text-caption text-muted-foreground tabular-nums">
												{tag.count}
											</span>
										) : null}
									</button>
								);
							})}
						</div>
					)}
					{tagFilter.length > 0 ? (
						<>
							<div className="my-0.5 h-px bg-border" />
							<button
								type="button"
								className="flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-muted-foreground text-xs hover:bg-muted/60 hover:text-foreground"
								onClick={clearTagFilter}
							>
								<X className="size-3" aria-hidden />
								{t("papersLibrary.clearTagFilter")}
							</button>
						</>
					) : null}
				</PopoverContent>
			</Popover>
		</div>
	);
}
