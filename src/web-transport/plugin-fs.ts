/**
 * Browser replacement for `@tauri-apps/plugin-fs`.
 *
 * Maps the eight plugin-fs calls the frontend actually makes onto the web
 * host file endpoints (contract agreed with the server handlers):
 *
 * | plugin-fs                    | web host                                  |
 * |------------------------------|-------------------------------------------|
 * | `readFile` / `readTextFile`  | `GET /api/file?path=`                     |
 * | `exists`                     | `GET /api/file/meta?path=` (200, `exists`) |
 * | `readDir`                    | `GET /api/dir?path=`                      |
 * | `writeFile` / `writeTextFile`| `PUT /api/file?path=`                     |
 * | `mkdir`                      | `POST /api/mkdir`                         |
 * | `remove`                     | `DELETE /api/file?path=&recursive=`       |
 *
 * Paths are absolute Vault paths, jailed server-side to the Vault root plus
 * `AGENTERO_WEB_ALLOW_ROOTS`. The shim throws an `Error` on failure, matching
 * plugin-fs rejection semantics.
 */

/** Entry shape returned by `readDir` (subset used by the frontend). */
export type DirEntry = {
	name: string;
	isFile: boolean;
	isDirectory: boolean;
	isSymlink: boolean;
};

export type WriteOptions = {
	baseDir?: string;
};

export type MkdirOptions = {
	recursive?: boolean;
	baseDir?: string;
};

export type RemoveOptions = {
	recursive?: boolean;
	baseDir?: string;
};

type WireDirEntry = {
	name?: unknown;
	isFile?: unknown;
	isDirectory?: unknown;
	isSymlink?: unknown;
};

type WireFileMeta = {
	exists?: unknown;
};

function fileUrl(path: string, extra?: Record<string, string>): string {
	const url = new URL("/api/file", window.location.origin);
	url.searchParams.set("path", path);
	if (extra) {
		for (const [key, value] of Object.entries(extra)) {
			url.searchParams.set(key, value);
		}
	}
	return url.toString();
}

function dirUrl(path: string): string {
	const url = new URL("/api/dir", window.location.origin);
	url.searchParams.set("path", path);
	return url.toString();
}

function metaUrl(path: string): string {
	const url = new URL("/api/file/meta", window.location.origin);
	url.searchParams.set("path", path);
	return url.toString();
}

async function failure(response: Response): Promise<Error> {
	let detail = "";
	try {
		const text = await response.text();
		if (text) {
			try {
				const body = JSON.parse(text) as { error?: unknown };
				detail = typeof body.error === "string" ? body.error : text;
			} catch {
				detail = text;
			}
		}
	} catch {
		// Unreadable body: status line only.
	}
	return new Error(
		`web host file request failed (HTTP ${response.status})${
			detail ? `: ${detail}` : ""
		}`,
	);
}

/** Read a Vault file as raw bytes. */
export async function readFile(path: string): Promise<Uint8Array> {
	const response = await fetch(fileUrl(path), { cache: "no-store" });
	if (!response.ok) throw await failure(response);
	return new Uint8Array(await response.arrayBuffer());
}

/** Read a Vault file as UTF-8 text. */
export async function readTextFile(path: string): Promise<string> {
	const response = await fetch(fileUrl(path), { cache: "no-store" });
	if (!response.ok) throw await failure(response);
	return response.text();
}

/** Whether a file *or directory* exists. Never throws (plugin-fs semantics). */
export async function exists(path: string): Promise<boolean> {
	try {
		const response = await fetch(metaUrl(path), { cache: "no-store" });
		if (!response.ok) return false;
		const body = (await response.json()) as WireFileMeta;
		return body.exists === true;
	} catch {
		return false;
	}
}

/** Write raw bytes; parent directories are created server-side. */
export async function writeFile(
	path: string,
	data: Uint8Array,
	_options?: WriteOptions,
): Promise<void> {
	const response = await fetch(fileUrl(path), {
		method: "PUT",
		headers: { "content-type": "application/octet-stream" },
		// Copy into a plain ArrayBuffer view: BodyInit rejects shared buffers.
		body: Uint8Array.from(data).buffer,
	});
	if (!response.ok) throw await failure(response);
}

/** Write UTF-8 text (or raw bytes); parent directories are created server-side. */
export async function writeTextFile(
	path: string,
	data: string | Uint8Array,
	_options?: WriteOptions,
): Promise<void> {
	if (typeof data !== "string") {
		await writeFile(path, data);
		return;
	}
	const response = await fetch(fileUrl(path), {
		method: "PUT",
		headers: { "content-type": "text/plain; charset=utf-8" },
		body: data,
	});
	if (!response.ok) throw await failure(response);
}

/**
 * Create a directory (recursive when requested), idempotent server-side.
 *
 * `POST /api/mkdir?path=<abs>&recursive=true|false` — the host parses both
 * values from the query string; the recursive default is `true` server-side.
 */
export async function mkdir(
	path: string,
	options?: MkdirOptions,
): Promise<void> {
	const url = new URL("/api/mkdir", window.location.origin);
	url.searchParams.set("path", path);
	url.searchParams.set(
		"recursive",
		options?.recursive === false ? "false" : "true",
	);
	const response = await fetch(url.toString(), { method: "POST" });
	if (!response.ok) throw await failure(response);
}

/**
 * Remove a file, or a directory when `recursive` is set.
 *
 * `DELETE /api/file?path=<abs>&recursive=true|false` — the host deserializes
 * `recursive` as a bool, so the query value must be the literal `true`/`false`.
 */
export async function remove(
	path: string,
	options?: RemoveOptions,
): Promise<void> {
	const response = await fetch(
		fileUrl(path, { recursive: options?.recursive ? "true" : "false" }),
		{ method: "DELETE" },
	);
	if (!response.ok) throw await failure(response);
}

/** List a directory (files and subdirectories). */
export async function readDir(path: string): Promise<DirEntry[]> {
	const response = await fetch(dirUrl(path), { cache: "no-store" });
	if (!response.ok) throw await failure(response);
	const body = (await response.json()) as unknown;
	if (!Array.isArray(body)) {
		throw new Error("web host /api/dir returned an invalid entry list");
	}
	return body.map((raw): DirEntry => {
		const entry = (raw ?? {}) as WireDirEntry;
		return {
			name: typeof entry.name === "string" ? entry.name : "",
			isFile: entry.isFile === true,
			isDirectory: entry.isDirectory === true,
			isSymlink: entry.isSymlink === true,
		};
	});
}
