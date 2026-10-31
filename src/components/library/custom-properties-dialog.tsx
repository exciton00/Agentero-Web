/**
 * Manage the Library's custom property columns (catalog schema v11).
 *
 * Opened from the Library header context menu. Definitions are edited here
 * (create / rename / option pool / delete); per-paper values are edited in the
 * table cells. All mutations go through the `custom_properties_*` Web host
 * RPCs and then ask the library to reload definitions + paper values.
 */
import { Pencil, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage, notifyError, notifySuccess } from "@/lib/core/notify";
import {
	type CustomProperty,
	type CustomPropertyOptionDraft,
	type CustomPropertyType,
	createCustomProperty,
	deleteCustomProperty,
	updateCustomProperty,
} from "@/lib/paper/custom-properties";
import { getVaultPath } from "@/lib/vault/store";

type CustomPropertiesDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	properties: CustomProperty[];
	/** Reload definitions + values after a successful mutation. */
	onChanged: () => void;
};

function parseOptionsDraft(text: string): CustomPropertyOptionDraft[] {
	return text
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean)
		.map((line) => {
			const [rawName, rawColor] = line.split(":");
			const name = rawName?.trim() ?? "";
			const color = rawColor?.trim() || undefined;
			return { name, color: color ?? null };
		})
		.filter((option) => option.name.length > 0);
}

function optionsToDraft(property: CustomProperty): string {
	return property.options
		.map((option) =>
			option.color ? `${option.name}:${option.color}` : option.name,
		)
		.join("\n");
}

export function CustomPropertiesDialog({
	open,
	onOpenChange,
	properties,
	onChanged,
}: CustomPropertiesDialogProps) {
	const { t } = useTranslation(["sidebar", "common"]);
	const [name, setName] = useState("");
	const [type, setType] = useState<CustomPropertyType>("select");
	const [optionsText, setOptionsText] = useState("");
	const [editingId, setEditingId] = useState<string | null>(null);
	const [editName, setEditName] = useState("");
	const [editOptionsText, setEditOptionsText] = useState("");
	const [busy, setBusy] = useState(false);
	const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

	const vaultPath = getVaultPath();

	const resetCreate = () => {
		setName("");
		setType("select");
		setOptionsText("");
	};

	const run = async (action: () => Promise<void>, successMessage?: string) => {
		if (busy) return;
		setBusy(true);
		try {
			await action();
			if (successMessage) notifySuccess(successMessage);
			onChanged();
		} catch (error) {
			notifyError(errorMessage(error));
		} finally {
			setBusy(false);
		}
	};

	const create = () =>
		run(async () => {
			if (!vaultPath)
				throw new Error(t("papersLibrary.customProperties.noVault"));
			await createCustomProperty(vaultPath, {
				name: name.trim(),
				type,
				options: type === "select" ? parseOptionsDraft(optionsText) : [],
			});
			resetCreate();
		}, t("papersLibrary.customProperties.created"));

	const startEdit = (property: CustomProperty) => {
		setEditingId(property.id);
		setEditName(property.name);
		setEditOptionsText(optionsToDraft(property));
		setConfirmDeleteId(null);
	};

	const saveEdit = (property: CustomProperty) =>
		run(async () => {
			if (!vaultPath)
				throw new Error(t("papersLibrary.customProperties.noVault"));
			await updateCustomProperty(vaultPath, {
				...property,
				name: editName.trim(),
				options:
					property.type === "select"
						? parseOptionsDraft(editOptionsText).map((option) => ({
								id:
									property.options.find(
										(existing) =>
											existing.name.toLowerCase() === option.name.toLowerCase(),
									)?.id ?? "",
								name: option.name,
								color: option.color ?? null,
							}))
						: property.options,
			});
			setEditingId(null);
		}, t("papersLibrary.customProperties.updated"));

	const remove = (property: CustomProperty) =>
		run(async () => {
			if (!vaultPath)
				throw new Error(t("papersLibrary.customProperties.noVault"));
			await deleteCustomProperty(vaultPath, property.id);
			setConfirmDeleteId(null);
		}, t("papersLibrary.customProperties.deleted"));

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) {
					setEditingId(null);
					setConfirmDeleteId(null);
					resetCreate();
				}
				onOpenChange(next);
			}}
		>
			<DialogContent className="sm:max-w-lg">
				<DialogHeader>
					<DialogTitle>{t("papersLibrary.customProperties.title")}</DialogTitle>
					<DialogDescription>
						{t("papersLibrary.customProperties.description")}
					</DialogDescription>
				</DialogHeader>

				<div className="flex max-h-[45vh] min-h-0 flex-col gap-1.5 overflow-y-auto pr-1">
					{properties.length === 0 ? (
						<p className="px-1 py-2 text-muted-foreground text-xs">
							{t("papersLibrary.customProperties.empty")}
						</p>
					) : (
						properties.map((property) =>
							editingId === property.id ? (
								<div
									key={property.id}
									className="flex flex-col gap-1.5 rounded-md border border-border/60 p-2"
								>
									<Input
										value={editName}
										onChange={(event) => setEditName(event.target.value)}
										placeholder={t("papersLibrary.customProperties.name")}
										className="h-7 text-xs"
									/>
									{property.type === "select" ? (
										<Textarea
											value={editOptionsText}
											onChange={(event) =>
												setEditOptionsText(event.target.value)
											}
											placeholder={t(
												"papersLibrary.customProperties.optionsHint",
											)}
											className="min-h-16 text-xs"
										/>
									) : null}
									<div className="flex justify-end gap-1.5">
										<Button
											type="button"
											size="sm"
											variant="ghost"
											disabled={busy}
											onClick={() => setEditingId(null)}
										>
											{t("papersLibrary.customProperties.cancel")}
										</Button>
										<Button
											type="button"
											size="sm"
											disabled={busy || !editName.trim()}
											onClick={() => saveEdit(property)}
										>
											{t("papersLibrary.customProperties.save")}
										</Button>
									</div>
								</div>
							) : (
								<div
									key={property.id}
									className="flex items-center gap-2 rounded-md px-1 py-1.5 hover:bg-muted/40"
								>
									<div className="flex min-w-0 flex-1 flex-col">
										<span className="truncate text-sm">{property.name}</span>
										<span className="truncate text-muted-foreground text-xs">
											{property.type === "select"
												? property.options
														.map((option) => option.name)
														.join(" · ") ||
													t("papersLibrary.customProperties.emptyOptions")
												: t("papersLibrary.customProperties.typeText")}
										</span>
									</div>
									<Button
										type="button"
										size="sm"
										variant="ghost"
										className="size-7 p-0"
										onClick={() => startEdit(property)}
										aria-label={t("papersLibrary.customProperties.edit")}
									>
										<Pencil className="size-3.5" aria-hidden />
									</Button>
									{confirmDeleteId === property.id ? (
										<Button
											type="button"
											size="sm"
											variant="destructive"
											disabled={busy}
											onClick={() => remove(property)}
										>
											{t("papersLibrary.customProperties.deleteConfirm")}
										</Button>
									) : (
										<Button
											type="button"
											size="sm"
											variant="ghost"
											className="size-7 p-0 text-muted-foreground hover:text-destructive"
											onClick={() => setConfirmDeleteId(property.id)}
											aria-label={t("papersLibrary.customProperties.delete")}
										>
											<Trash2 className="size-3.5" aria-hidden />
										</Button>
									)}
								</div>
							),
						)
					)}
				</div>

				<div className="flex flex-col gap-1.5 border-border/60 border-t pt-3">
					<div className="flex items-center gap-1.5">
						<Input
							value={name}
							onChange={(event) => setName(event.target.value)}
							placeholder={t("papersLibrary.customProperties.namePlaceholder")}
							className="h-8 flex-1 text-sm"
						/>
						<Select
							value={type}
							onValueChange={(value) => {
								setType(value as CustomPropertyType);
								if (value === "text") setOptionsText("");
							}}
						>
							<SelectTrigger size="sm" className="w-28">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="select">
									{t("papersLibrary.customProperties.typeSelect")}
								</SelectItem>
								<SelectItem value="text">
									{t("papersLibrary.customProperties.typeText")}
								</SelectItem>
							</SelectContent>
						</Select>
						<Button
							type="button"
							size="sm"
							disabled={busy || !name.trim()}
							onClick={create}
						>
							<Plus className="size-3.5" aria-hidden />
							{t("papersLibrary.customProperties.add")}
						</Button>
					</div>
					{type === "select" ? (
						<Textarea
							value={optionsText}
							onChange={(event) => setOptionsText(event.target.value)}
							placeholder={t("papersLibrary.customProperties.optionsHint")}
							className="min-h-14 text-xs"
						/>
					) : null}
				</div>

				<DialogFooter>
					<Button
						type="button"
						variant="outline"
						onClick={() => onOpenChange(false)}
					>
						{t("papersLibrary.customProperties.close")}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
