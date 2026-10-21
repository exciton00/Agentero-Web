/**
 * Browser replacement for `@tauri-apps/api/core`.
 *
 * Aliased in `vite.config.ts` only for `--mode web`; the desktop build keeps
 * importing the real Tauri package. `invoke` becomes an HTTP RPC call and
 * `Channel` keeps the shape the generated `bindings.ts` constructs.
 */

import { Channel } from "./channel";
import { rpc } from "./rpc";

export { Channel, rpc };

/** Arguments object accepted by Tauri's `invoke` (kept for type parity). */
export type InvokeArgs =
	| Record<string, unknown>
	| number[]
	| ArrayBuffer
	| Uint8Array;

/** Key marker used by Tauri to customize IPC serialization. */
export const SERIALIZE_TO_IPC_FN = "__TAURI_TO_IPC_KEY__";

/**
 * Call a Host command: `POST /api/rpc/<command>` with the same arguments the
 * desktop IPC would send and the same JSON envelope back.
 */
export function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
	return rpc<T>(command, args);
}

/**
 * Register a JS callback the backend could invoke by id. The web host does not
 * call callbacks (it has no `eval` bridge), but the global internals polyfill
 * expects the symbol to exist.
 */
export function transformCallback<T = unknown>(
	callback?: (response: T) => void,
	_once = false,
): number {
	callbackIds.push(
		callback ? (response: unknown) => callback(response as T) : undefined,
	);
	return callbackIds.length - 1;
}

const callbackIds: Array<((response: unknown) => void) | undefined> = [];

/** Unregister a callback created by {@link transformCallback}. */
export function unregisterCallback(id: number): void {
	callbackIds[id] = undefined;
}
