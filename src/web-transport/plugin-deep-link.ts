/**
 * Browser replacement for `@tauri-apps/plugin-deep-link`.
 *
 * Deep links open the desktop app from an `agentero://` URL; a self-hosted web
 * UI at its own origin does not receive OS deep links. Both APIs resolve to
 * "nothing to handle" so mobile/bridge code paths stay inert.
 */

/** No deep-link URL is pending in a normal browser tab. */
export async function getCurrent(): Promise<string[] | null> {
	return null;
}

/** Deep links never arrive; the returned disposer is a no-op. */
export function onOpenUrl(
	_handler: (urls: string[]) => void,
): Promise<() => void> {
	return Promise.resolve(() => undefined);
}
