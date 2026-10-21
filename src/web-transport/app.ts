/**
 * Browser replacement for `@tauri-apps/api/app`.
 *
 * The version shown in Settings → About comes from `package.json` at build
 * time (`__APP_VERSION__`, provided by the web-mode Vite config), so the web
 * build reports the same version as the desktop shell.
 */

declare const __APP_VERSION__: string;

const VERSION: string =
	typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "web";

/** Application version (from `package.json` at build time). */
export function getVersion(): Promise<string> {
	return Promise.resolve(VERSION);
}

/** Product name; matches the bundle identifier's display name. */
export function getName(): Promise<string> {
	return Promise.resolve("Agentero");
}

/** Tauri runtime version; the web host is not a Tauri runtime. */
export function getTauriVersion(): Promise<string> {
	return Promise.resolve("2");
}
