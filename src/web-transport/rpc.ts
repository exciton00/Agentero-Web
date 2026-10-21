/**
 * HTTP RPC transport for the headless web host.
 *
 * Mirrors the desktop `invoke()` contract (see `docs/development/webui.md`
 * §2.2): `POST /api/rpc/<command>` with the command's arguments as the JSON
 * body, answered with exactly the JSON the desktop command would return.
 *
 * Error semantics matter: Tauri rejects `invoke()` with the command's
 * serialized `Err` value, which for `Result<_, String>` commands is a plain
 * string. The generated `typedError` wrapper in `bindings.ts` relies on that
 * (`if (e instanceof Error) throw e; return { status: "error", error: e }`),
 * so a non-2xx response is rejected with the error *string*, not an `Error`.
 */

import { getWindowLabel } from "./label";

/** Envelope of the host's transport-level error responses. */
type ErrorBody = { error?: unknown };

/** HTTP RPC client shared by every web transport shim. */
export async function rpc<T>(command: string, args?: unknown): Promise<T> {
	let response: Response;
	try {
		response = await fetch(`/api/rpc/${encodeURIComponent(command)}`, {
			method: "POST",
			headers: {
				"content-type": "application/json",
				"x-agentero-window": getWindowLabel(),
			},
			body: JSON.stringify(args ?? {}),
		});
	} catch (error) {
		// Transport failure (server down / offline): surface as a real Error so
		// callers can distinguish it from a Host command rejection.
		throw new Error(
			`Cannot reach the Agentero web host: ${
				error instanceof Error ? error.message : String(error)
			}`,
		);
	}

	if (!response.ok) {
		throw await transportError(response);
	}

	const text = await response.text();
	if (!text) return undefined as T;
	try {
		return JSON.parse(text) as T;
	} catch {
		throw new Error(
			`Host returned invalid JSON for command "${command}": ${text.slice(0, 200)}`,
		);
	}
}

async function transportError(response: Response): Promise<string> {
	let detail = "";
	try {
		const text = await response.text();
		if (text) {
			try {
				const body = JSON.parse(text) as ErrorBody;
				detail = typeof body.error === "string" ? body.error : text;
			} catch {
				detail = text;
			}
		}
	} catch {
		// Body already consumed / unreadable: fall back to the status line.
	}
	return detail || `Host command failed (HTTP ${response.status})`;
}
