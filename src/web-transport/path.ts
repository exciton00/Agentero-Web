/**
 * Browser replacement for `@tauri-apps/api/path`.
 *
 * Pure path string operations (`join`, `dirname`) keep working client-side.
 * Host-directory lookups (`homeDir`, `appDataDir`, …) describe the *server's*
 * filesystem, which the browser must not guess: they reject, and callers are
 * expected to be web-host-aware (settings → About log folder, Zotero migration
 * — both desktop-only features).
 */

/** POSIX-style join, mirroring Tauri's `PathBuf` push semantics. */
export function join(...paths: string[]): Promise<string> {
	let result = "";
	for (const raw of paths) {
		if (!raw) continue;
		if (raw.startsWith("/")) {
			result = raw;
			continue;
		}
		if (!result) {
			result = raw;
			continue;
		}
		result = `${result.replace(/\/+$/, "")}/${raw}`;
	}
	return Promise.resolve(result);
}

/** POSIX-style dirname (no trailing separator, root stays `/`). */
export function dirname(path: string): Promise<string> {
	const trimmed = path.replace(/\/+$/, "");
	if (!trimmed) return Promise.resolve("/");
	const index = trimmed.lastIndexOf("/");
	if (index < 0) return Promise.resolve(".");
	if (index === 0) return Promise.resolve("/");
	return Promise.resolve(trimmed.slice(0, index));
}

/** POSIX-style basename. */
export function basename(path: string, ext?: string): Promise<string> {
	const trimmed = path.replace(/\/+$/, "");
	const name = trimmed.slice(trimmed.lastIndexOf("/") + 1);
	if (!ext || !name.endsWith(ext) || name === ext) {
		return Promise.resolve(name);
	}
	return Promise.resolve(name.slice(0, -ext.length));
}

/** Whether the path is absolute (POSIX form; the web host runs on Linux). */
export function isAbsolute(path: string): Promise<boolean> {
	return Promise.resolve(path.startsWith("/"));
}

/** Path separator used by Host-side paths. */
export const sep = "/";

/** Path-list separator used by Host-side paths. */
export const delimiter = ":";

/** Host-side paths are not exposed to the browser. */
function unavailable(name: string): Promise<string> {
	return Promise.reject(
		new Error(
			`@tauri-apps/api/path ${name}() is not available in the headless web host`,
		),
	);
}

export function homeDir(): Promise<string> {
	return unavailable("homeDir");
}

export function appDataDir(): Promise<string> {
	return unavailable("appDataDir");
}

export function appConfigDir(): Promise<string> {
	return unavailable("appConfigDir");
}

export function appLogDir(): Promise<string> {
	return unavailable("appLogDir");
}

export function configDir(): Promise<string> {
	return unavailable("configDir");
}

export function dataDir(): Promise<string> {
	return unavailable("dataDir");
}
