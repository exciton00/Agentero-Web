/**
 * Native settings window control (single instance; `?window=settings` route).
 */

import { commands } from "@/lib/core/bindings";
import { notifyError } from "@/lib/core/notify";
import { isTauri, isWebHost } from "@/lib/core/tauri";
import { getVaultPath } from "@/lib/vault/store";
import { setSettingsOpenState, uiStore } from "./ui-store";

export function openSettingsWindow(section: string = "general"): void {
	if (isWebHost()) {
		// The web build has no native windows: settings are a separate browser
		// tab using the existing `?window=settings` shell route.
		const url = new URL(window.location.href);
		const vaultPath = getVaultPath();
		url.search = `?window=settings&section=${encodeURIComponent(section)}`;
		if (vaultPath) {
			url.searchParams.set("vault_path", vaultPath);
		}
		window.open(url.toString(), "_blank", "noopener");
		return;
	}
	if (!isTauri()) return;
	void (async () => {
		try {
			const res = await commands.settingsWindowOpen(section, getVaultPath());
			if (res.status === "error") {
				notifyError(res.error);
				return;
			}
			setSettingsOpenState(true);
		} catch (e) {
			notifyError(String(e));
		}
	})();
}

export function closeSettingsWindow(): void {
	if (isWebHost()) {
		window.close();
		return;
	}
	if (!isTauri()) return;
	void (async () => {
		try {
			const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
			const win = await WebviewWindow.getByLabel("settings");
			await win?.close();
		} catch {
			// ignore
		}
	})();
}

export function toggleSettingsWindow(): void {
	if (uiStore.getState().settingsOpen) closeSettingsWindow();
	else openSettingsWindow();
}
