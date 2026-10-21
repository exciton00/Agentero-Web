/**
 * Browser replacement for `@tauri-apps/plugin-dialog`.
 *
 * The browser has no native file dialog for *server-side* directories: the
 * Host file picker in the desktop app returns absolute local paths, which do
 * not exist in the browser. Returning `null` means "user cancelled", the
 * branch every caller already handles. A server-side directory browser lands
 * in M1; until then Vault creation/opening is surfaced as a desktop-only
 * action by the callers.
 */

/** Open-dialog result: `null` = cancelled (no server-side picker yet). */
export async function open(_options?: unknown): Promise<null> {
	return null;
}

/** Save-dialog result: `null` = cancelled (no server-side picker yet). */
export async function save(_options?: unknown): Promise<null> {
	return null;
}

/** Message dialogs are not implemented in the browser (use in-app toasts). */
export async function message(
	_message: string,
	_options?: unknown,
): Promise<void> {
	// No-op: the app uses its own toast stack.
}

export async function ask(
	_message: string,
	_options?: unknown,
): Promise<boolean> {
	return false;
}

export async function confirm(
	_message: string,
	_options?: unknown,
): Promise<boolean> {
	return false;
}
