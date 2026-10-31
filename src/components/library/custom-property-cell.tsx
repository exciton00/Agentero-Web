/**
 * One custom-property cell in the papers library table.
 *
 * `select` properties open a small popover with the option pool, quick
 * "create option and assign" input and a clear action; `text` properties edit
 * inline (Enter/blur commits, Esc cancels, blank clears). Both editors stop
 * click propagation so interacting with a cell never selects/opens the row.
 */
import { Check, Plus, X } from "lucide-react";
import { memo, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/components/ui/popover";
import type { PaperMetadata } from "@/lib/paper";
import type { CustomProperty } from "@/lib/paper/custom-properties";
import {
	isTagColorId,
	tagChipStyle,
	tagSwatchStyle,
} from "@/lib/ui/tag-colors";

export type CustomPropertyCellProps = {
	property: CustomProperty;
	/** Current raw value: option id for `select`, text for `text`. */
	value: string | null;
	paper: PaperMetadata;
	onSetValue?: (
		paper: PaperMetadata,
		propertyId: string,
		value: string | null,
	) => void;
	onCreateOption?: (
		paper: PaperMetadata,
		propertyId: string,
		name: string,
		color?: string | null,
	) => void;
};

const CELL_CLASS = "min-w-0 max-w-0 overflow-hidden px-3 py-2.5";

export const CustomPropertyCell = memo(function CustomPropertyCell(
	props: CustomPropertyCellProps,
) {
	return props.property.type === "text" ? (
		<TextPropertyCell {...props} />
	) : (
		<SelectPropertyCell {...props} />
	);
});

function SelectPropertyCell({
	property,
	value,
	paper,
	onSetValue,
	onCreateOption,
}: CustomPropertyCellProps) {
	const { t } = useTranslation("sidebar");
	const [open, setOpen] = useState(false);
	const [draft, setDraft] = useState("");
	const [busy, setBusy] = useState(false);
	const selected = value
		? property.options.find((option) => option.id === value)
		: undefined;

	const createOption = () => {
		const name = draft.trim();
		if (!name || busy || !onCreateOption) return;
		setBusy(true);
		try {
			onCreateOption(paper, property.id, name);
			setDraft("");
			setOpen(false);
		} finally {
			setBusy(false);
		}
	};

	return (
		<td className={CELL_CLASS}>
			<Popover
				open={open}
				onOpenChange={(next) => {
					setOpen(next);
					if (!next) setDraft("");
				}}
			>
				<PopoverTrigger asChild>
					<button
						type="button"
						onClick={(event) => event.stopPropagation()}
						className="flex w-full min-w-0 items-center text-left"
						title={
							selected?.name ?? t("papersLibrary.customProperties.setValue")
						}
					>
						{selected ? (
							<span
								className="truncate rounded px-1.5 py-0.5 text-[11px]"
								style={tagChipStyle(
									isTagColorId(selected.color) ? selected.color : null,
								)}
							>
								{selected.name}
							</span>
						) : (
							<span className="text-muted-foreground text-xs">—</span>
						)}
					</button>
				</PopoverTrigger>
				<PopoverContent
					align="start"
					className="w-60 p-1.5"
					onClick={(event) => event.stopPropagation()}
				>
					<div className="max-h-56 overflow-y-auto">
						{property.options.length === 0 ? (
							<p className="px-2 py-1.5 text-muted-foreground text-xs">
								{t("papersLibrary.customProperties.emptyOptions")}
							</p>
						) : (
							property.options.map((option) => (
								<button
									key={option.id}
									type="button"
									className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-muted/60"
									onClick={() => {
										onSetValue?.(paper, property.id, option.id);
										setOpen(false);
									}}
								>
									<span
										className="size-2 shrink-0 rounded-full border border-border/40"
										style={tagSwatchStyle(
											isTagColorId(option.color) ? option.color : null,
										)}
									/>
									<span className="min-w-0 flex-1 truncate">{option.name}</span>
									{option.id === value ? (
										<Check className="size-3.5 shrink-0" aria-hidden />
									) : null}
								</button>
							))
						)}
					</div>
					{onCreateOption ? (
						<div className="mt-1 flex items-center gap-1 border-border/60 border-t px-1 pt-1.5">
							<Input
								value={draft}
								onChange={(event) => setDraft(event.target.value)}
								onKeyDown={(event) => {
									if (event.key === "Enter") {
										event.preventDefault();
										createOption();
									}
								}}
								placeholder={t("papersLibrary.customProperties.newOption")}
								className="h-7 flex-1 text-xs"
								onClick={(event) => event.stopPropagation()}
							/>
							<Button
								type="button"
								size="sm"
								variant="ghost"
								className="size-7 shrink-0 p-0"
								disabled={busy || !draft.trim()}
								onClick={createOption}
								aria-label={t("papersLibrary.customProperties.newOption")}
							>
								<Plus className="size-3.5" aria-hidden />
							</Button>
						</div>
					) : null}
					{value ? (
						<button
							type="button"
							className="mt-1 flex w-full items-center gap-1.5 rounded px-2 py-1.5 text-muted-foreground text-xs hover:bg-muted/60 hover:text-foreground"
							onClick={() => {
								onSetValue?.(paper, property.id, null);
								setOpen(false);
							}}
						>
							<X className="size-3" aria-hidden />
							{t("papersLibrary.customProperties.clearValue")}
						</button>
					) : null}
				</PopoverContent>
			</Popover>
		</td>
	);
}

function TextPropertyCell({
	property,
	value,
	paper,
	onSetValue,
}: CustomPropertyCellProps) {
	const { t } = useTranslation("sidebar");
	const [editing, setEditing] = useState(false);
	const [draft, setDraft] = useState(value ?? "");
	const inputRef = useRef<HTMLInputElement>(null);

	useEffect(() => {
		if (editing) inputRef.current?.focus();
	}, [editing]);

	const commit = () => {
		setEditing(false);
		const next = draft.trim();
		if (next === (value ?? "")) return;
		onSetValue?.(paper, property.id, next ? next : null);
	};

	if (!editing) {
		return (
			<td className={CELL_CLASS}>
				<button
					type="button"
					onClick={(event) => {
						event.stopPropagation();
						setDraft(value ?? "");
						setEditing(true);
					}}
					className="block w-full truncate text-left text-xs"
					title={value ?? t("papersLibrary.customProperties.setValue")}
				>
					{value ? value : <span className="text-muted-foreground">—</span>}
				</button>
			</td>
		);
	}
	return (
		<td className={CELL_CLASS}>
			<Input
				ref={inputRef}
				value={draft}
				onChange={(event) => setDraft(event.target.value)}
				onClick={(event) => event.stopPropagation()}
				onBlur={commit}
				onKeyDown={(event) => {
					if (event.key === "Enter") {
						event.preventDefault();
						commit();
					} else if (event.key === "Escape") {
						event.preventDefault();
						setEditing(false);
					}
				}}
				className="h-7 text-xs"
			/>
		</td>
	);
}
