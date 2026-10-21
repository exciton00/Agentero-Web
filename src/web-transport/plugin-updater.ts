/**
 * Browser replacement for `@tauri-apps/plugin-updater`.
 *
 * Automatic updates are a desktop packaging feature and explicitly out of
 * scope for the headless web host (docs/development/webui.md §1). `check()`
 * answers "no update available" instead of throwing so the Settings → About
 * pane still renders; the actual update/install actions are guarded by
 * `isTauri()` on the desktop side.
 */

export type DownloadEvent =
	| { event: "Started"; data: { contentLength?: number } }
	| { event: "Progress"; data: { chunkLength: number } }
	| { event: "Finished" };

/** Subset of the plugin's `Update` used by `src/lib/update/service.ts`. */
export type Update = {
	currentVersion: string;
	version: string;
	body?: string;
	date?: string;
	downloadAndInstall: (
		onEvent?: (event: DownloadEvent) => void,
	) => Promise<void>;
	install: () => Promise<void>;
	close: () => Promise<void>;
};

/** The web host has no updater service: report "up to date". */
export async function check(_options?: unknown): Promise<Update | null> {
	return null;
}
