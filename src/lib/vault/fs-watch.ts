import { commands, type VaultFileChangedEvent } from "@/lib/core/bindings";
import { callResult } from "@/lib/core/ipc";
import { hasHostTransport } from "@/lib/core/tauri";
import { isRemoteVaultHandle } from "@/lib/vault/remote/remote-vault";

/**
 * Payload of the `vault:file-changed` event emitted by the Host watcher.
 * Wire shape from the generated bindings; subscribe via `events.vaultFileChanged`.
 */
export type VaultFileChangedPayload = VaultFileChangedEvent;

/** Start (or restart) watching the given Vault directory for this window. */
export async function startVaultWatch(vaultPath: string): Promise<void> {
	if (!hasHostTransport() || !vaultPath) return;
	// Remote vaults (`remote:<sessionId>`) have no local notify path.
	if (isRemoteVaultHandle(vaultPath)) return;
	await callResult(() => commands.fsWatchStart(vaultPath));
}

/** Stop watching the Vault for this window. */
export async function stopVaultWatch(): Promise<void> {
	if (!hasHostTransport()) return;
	await callResult(() => commands.fsWatchStop());
}
