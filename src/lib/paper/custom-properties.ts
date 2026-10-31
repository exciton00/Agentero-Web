/**
 * Paper-library custom properties (catalog schema v11): user defined
 * `select` / `text` columns stored in the Vault catalog.
 *
 * The Web host exposes plain `ApiResult<T>` RPCs (`custom_properties_*`) that
 * `bindings.ts` does not know about yet, so this module invokes them by name
 * through the same transport the generated bindings use — the same pattern as
 * the RAG API module. Property definitions live in `.agentero/properties.json`
 * + `custom_properties`; per-paper values live in `papers.custom_props_json`.
 */

import { invoke } from "@tauri-apps/api/core";
import type { ApiResult } from "@/lib/core/bindings";
import { hasHostTransport } from "@/lib/core/tauri";

export type CustomPropertyType = "select" | "text";

export type CustomPropertyOption = {
	id: string;
	name: string;
	/** Tag palette color id (see `lib/ui/tag-colors`), absent for default chips. */
	color?: string | null;
};

/** One property definition (mirrors the Rust `CustomProperty` DTO). */
export type CustomProperty = {
	id: string;
	name: string;
	type: CustomPropertyType;
	orderIndex: number;
	options: CustomPropertyOption[];
	createdAt: string;
	updatedAt: string;
};

/** Draft option accepted when creating/updating a definition. */
export type CustomPropertyOptionDraft = {
	/** Existing option id; absent for a new option. */
	id?: string;
	name: string;
	color?: string | null;
};

/** Raw text of a text property, or the option id of a select property. */
export type CustomPropertyValues = Record<string, string>;
/** `{paperPath -> {propertyId -> value}}` for every paper with assignments. */
export type CustomPropertyValuesByPath = Record<string, CustomPropertyValues>;

export const CUSTOM_PROPERTIES_NO_HOST_MESSAGE =
	"Custom properties require the Agentero Host (web host RPC).";

async function call<T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> {
	if (!hasHostTransport()) throw new Error(CUSTOM_PROPERTIES_NO_HOST_MESSAGE);
	const response = await invoke<ApiResult<T>>(command, args ?? {});
	if (!response || typeof response !== "object" || response.ok !== true) {
		const error = (
			response as
				| { error?: { message?: string; code?: string } | null }
				| null
				| undefined
		)?.error;
		throw new Error(
			error?.message ||
				error?.code ||
				`Custom property command failed: ${command}`,
		);
	}
	return response.data as T;
}

/** All custom property definitions for the Vault. */
export async function listCustomProperties(
	vaultPath: string,
): Promise<CustomProperty[]> {
	const response = await call<{ properties: CustomProperty[] }>(
		"custom_properties_list",
		{ vaultPath },
	);
	return Array.isArray(response?.properties) ? response.properties : [];
}

/** `{paperPath -> {propertyId -> value}}` for every paper with assignments. */
export async function customPropertyValuesByPath(
	vaultPath: string,
): Promise<CustomPropertyValuesByPath> {
	const response = await call<{ valuesByPath: CustomPropertyValuesByPath }>(
		"custom_properties_paper_values",
		{ vaultPath },
	);
	return response?.valuesByPath ?? {};
}

/** Create a definition; `options` applies to `select` properties only. */
export async function createCustomProperty(
	vaultPath: string,
	draft: {
		name: string;
		type: CustomPropertyType;
		options?: CustomPropertyOptionDraft[];
	},
): Promise<CustomProperty> {
	return call<CustomProperty>("custom_properties_create", {
		vaultPath,
		name: draft.name,
		propertyType: draft.type,
		options: draft.options ?? [],
	});
}

/** Rename a definition or replace its option pool (type is immutable). */
export async function updateCustomProperty(
	vaultPath: string,
	property: CustomProperty,
): Promise<CustomProperty> {
	return call<CustomProperty>("custom_properties_update", {
		vaultPath,
		property,
	});
}

/** Delete a definition and clear its value on every paper. */
export async function deleteCustomProperty(
	vaultPath: string,
	propertyId: string,
): Promise<void> {
	await call<null>("custom_properties_delete", { vaultPath, propertyId });
}

/** Set (`value`) or clear (`null`) one paper's value. */
export async function setPaperCustomPropertyValue(
	vaultPath: string,
	paperRef: string,
	propertyId: string,
	value: string | null,
): Promise<{ path: string; values: CustomPropertyValues }> {
	return call<{ path: string; values: CustomPropertyValues }>(
		"custom_properties_set_paper_value",
		{ vaultPath, paperRef, propertyId, value },
	);
}

/** Create (or reuse) a select option and assign it to one paper. */
export async function createCustomPropertyOption(
	vaultPath: string,
	paperRef: string,
	propertyId: string,
	optionName: string,
	color?: string | null,
): Promise<{
	property: CustomProperty;
	path: string;
	values: CustomPropertyValues;
}> {
	return call<{
		property: CustomProperty;
		path: string;
		values: CustomPropertyValues;
	}>("custom_properties_create_option", {
		vaultPath,
		paperRef,
		propertyId,
		optionName,
		color,
	});
}

/** Remove one option from a select property and clear it from every paper. */
export async function deleteCustomPropertyOption(
	vaultPath: string,
	propertyId: string,
	optionId: string,
): Promise<CustomProperty> {
	return call<CustomProperty>("custom_properties_delete_option", {
		vaultPath,
		propertyId,
		optionId,
	});
}

/** Resolve a stored cell value to its option (select) or the raw text. */
export function customPropertyDisplayValue(
	property: CustomProperty,
	value: string | null | undefined,
): string {
	if (!value) return "";
	if (property.type === "text") return value;
	return property.options.find((option) => option.id === value)?.name ?? "";
}
