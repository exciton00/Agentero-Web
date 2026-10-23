/**
 * Loopback MCP server control (Host :8765 by default).
 * @see docs/backend/mcp.md
 */

import { commands } from "@/lib/core/bindings";
import { callApi, callApiResult } from "@/lib/core/ipc";
import { hasHostTransport } from "@/lib/core/tauri";

export const DEFAULT_MCP_PORT = 8765;

export type McpStatus = {
	enabled: boolean;
	listening: boolean;
	port: number;
	url: string | null;
	lastError: string | null;
	vaultPath: string | null;
};

const idle = (port = DEFAULT_MCP_PORT): McpStatus => ({
	enabled: false,
	listening: false,
	port,
	url: null,
	lastError: null,
	vaultPath: null,
});

export async function mcpGetStatus(): Promise<McpStatus> {
	if (!hasHostTransport()) return idle();
	return callApi(() => commands.mcpGetStatus());
}

export async function mcpSetEnabled(enabled: boolean): Promise<McpStatus> {
	if (!hasHostTransport()) return idle();
	return callApiResult(() => commands.mcpSetEnabled({ enabled }));
}

export async function mcpSetPort(port: number): Promise<McpStatus> {
	if (!hasHostTransport()) return idle(port);
	return callApiResult(() => commands.mcpSetPort({ port }));
}

export async function mcpSetVault(vaultPath: string | null): Promise<void> {
	if (!hasHostTransport()) return;
	await callApi(() => commands.mcpSetVault({ vaultPath }));
}

/** Sync recent local vaults so MCP `vault_list` can target any of them. */
export async function mcpSetKnownVaults(vaults: string[]): Promise<void> {
	if (!hasHostTransport()) return;
	await callApi(() => commands.mcpSetKnownVaults({ vaults }));
}

export async function mcpSetParentDir(parentDir: string): Promise<void> {
	if (!hasHostTransport()) return;
	const dir = parentDir
		.trim()
		.replace(/\\/g, "/")
		.replace(/^\/+|\/+$/g, "");
	if (!dir) return;
	await callApi(() => commands.mcpSetParentDir({ parentDir: dir }));
}
